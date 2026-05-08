import * as Dialog from "@radix-ui/react-dialog";
import { AlertTriangle, Image as ImageIcon, Loader2, RotateCcw, Sparkles, X } from "lucide-react";
import { useEffect, useState } from "react";
import { Link, useLocation, useNavigate, useParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
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
import { useApplyStream } from "@/hooks/useApplyStream";
import { useResizableWidth } from "@/hooks/useResizableWidth";
import type {
  Album,
  CoverSuggestionResponse,
  JobConflictDetail,
  JobType,
  MetadataSuggestionResponse,
} from "@/lib/types";

interface DetailSheetProps {
  /** The set of albums currently in state. The sheet looks up by URL :albumId. */
  albums: Album[];
  /** Called when the album mutates (apply succeeded) so the parent can update its tile. */
  onAlbumUpdated?: (album: Album) => void;
}

/**
 * Album detail sheet — slides in from the right when the URL is /library/:id.
 * On close, navigates back to /library.
 */
export default function DetailSheet({ albums, onAlbumUpdated }: DetailSheetProps) {
  const { albumId } = useParams<{ albumId: string }>();
  const navigate = useNavigate();
  const location = useLocation();
  const album = albumId ? albums.find((a) => a.id === albumId) : undefined;
  const open = !!album;

  // Drag-to-resize. Width persisted across sessions via localStorage.
  const { width, resizing, onPointerDown } = useResizableWidth({
    storageKey: "mm.sheet.width",
    defaultWidth: 480,
    minWidth: 400,
    maxWidth: 1200,
  });

  // Lifted apply-in-flight flag so the Dialog.Root can refuse close attempts
  // while the apply stream is mid-write. Without this, a user who instinctively
  // hits Escape or clicks outside loses the SSE stream and the album won't
  // refresh until the next scan. SheetBody sets this via setIsApplying based
  // on its own useApplyStream state.
  const [isApplying, setIsApplying] = useState(false);

  return (
    <Dialog.Root
      open={open}
      onOpenChange={(o) => {
        if (o) return;
        if (isApplying) {
          // Don't close while writing — fire a one-time toast so the user
          // understands why their click was ignored.
          toast.warning("Hold on — Media Manager is still writing changes.", {
            description: "The sheet will stay open until apply finishes.",
            duration: 3500,
          });
          return;
        }
        // Preserve any ?filter= / ?q= so closing the sheet returns the user
        // to the same filtered/searched view they came from.
        navigate(`/library${location.search}`);
      }}
    >
      <Dialog.Portal>
        <Dialog.Overlay className="sheet-overlay fixed inset-0 z-40 bg-black/35 backdrop-blur-[1px]" />
        <Dialog.Content
          className="sheet-content fixed top-0 right-0 bottom-0 z-50 max-w-[95vw] bg-panel border-l border-line shadow-sheet outline-none flex flex-col"
          style={{ width: `${width}px` }}
          aria-describedby={undefined}
          onEscapeKeyDown={(e) => { if (isApplying) e.preventDefault(); }}
          onPointerDownOutside={(e) => { if (isApplying) e.preventDefault(); }}
          onInteractOutside={(e) => { if (isApplying) e.preventDefault(); }}
        >
          {/* Drag handle on the left edge — anchored absolutely so it sits
              on top of the border without affecting flex layout. Wider hit
              area than the visual hairline so it's easy to grab. */}
          <div
            className={`absolute top-0 left-0 bottom-0 w-1.5 -translate-x-1/2 cursor-col-resize z-10 group ${
              resizing ? "" : "transition-colors"
            }`}
            onPointerDown={onPointerDown}
            role="separator"
            aria-orientation="vertical"
            aria-label="Resize panel"
            title="Drag to resize"
          >
            {/* Hairline that highlights on hover / during drag */}
            <div
              className={`absolute top-0 bottom-0 left-1/2 -translate-x-1/2 w-px ${
                resizing ? "bg-accent w-0.5" : "bg-transparent group-hover:bg-accent/60"
              }`}
            />
          </div>
          {album ? (
            <SheetBody album={album} onAlbumUpdated={onAlbumUpdated} setIsApplying={setIsApplying} />
          ) : (
            <Dialog.Title className="sr-only">Album details</Dialog.Title>
          )}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function SheetBody({
  album,
  onAlbumUpdated,
  setIsApplying,
}: {
  album: Album;
  onAlbumUpdated?: (a: Album) => void;
  setIsApplying: (v: boolean) => void;
}) {
  const tracks = useTracks(album.id);
  const apply = useApplyStream();
  const qc = useQueryClient();
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

  // Whether the album has any cover at all — drives auto-bundling. If
  // "Suggest fixes" is clicked on a coverless album, we enqueue a
  // metadata+cover job instead of metadata-only.
  const albumHasCover = album.has_embedded_cover || !!album.cover_file;

  // Hint dialog state. `pendingType` is the job type the user is about to
  // enqueue once they submit a hint.
  const [pendingType, setPendingType] = useState<JobType | null>(null);

  // Reset per-album local state when switching albums.
  useEffect(() => {
    setPendingType(null);
    apply.reset();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [album.id]);

  // When apply finishes successfully, propagate the updated album to the
  // parent (so the tile re-renders with new cover/title/etc.) and mark the
  // job as acted-on (action="applied") so it disappears from the active
  // queue and the "needs review" badge.
  useEffect(() => {
    if (apply.state.status === "done" && apply.state.updatedAlbum) {
      onAlbumUpdated?.(apply.state.updatedAlbum);
      qc.invalidateQueries({ queryKey: ["tracks", album.id] });
      if (activeJob) {
        markActedOn.mutate({ jobId: activeJob.id, action: "applied" });
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [apply.state.status, apply.state.updatedAlbum, onAlbumUpdated, qc, album.id]);

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
  const isApplying =
    apply.state.status === "writing" || apply.state.status === "downloading_cover";
  // Lift the in-flight flag so DetailSheet can refuse close attempts while
  // the apply stream is mid-write. Reset on unmount so a never-finished apply
  // doesn't strand the sheet open.
  useEffect(() => {
    setIsApplying(isApplying);
    return () => setIsApplying(false);
  }, [isApplying, setIsApplying]);
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

  // Live album for hero — use the apply-updated version once available.
  const displayAlbum: Album = apply.state.updatedAlbum ?? album;

  // ---- Click handlers ----

  const onSuggestMetadata = () => {
    // Auto-bundle cover lookup when album has no cover.
    setPendingType(albumHasCover ? "metadata" : "metadata+cover");
  };
  const onSuggestCover = () => setPendingType("cover");

  const onHintSubmit = async (hint: string) => {
    const which = pendingType;
    setPendingType(null);
    if (!which) return;
    try {
      await enqueue.mutateAsync({ albumId: album.id, jobType: which, hint });
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
        toast.error("Could not queue job", { duration: 4000 });
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
    await apply.start(album.id, {
      suggestions: metaResult?.suggestions ?? {},
      track_suggestions: trackSuggestions,
      cover_url: coverResult?.cover_url ?? null,
    });
    // Job is marked acted-on in the apply.state.status === "done" effect above.
  };

  const onDiscard = () => {
    if (activeJob) {
      markActedOn.mutate({ jobId: activeJob.id, action: "discarded" });
    }
  };

  return (
    <>
      {/* Sheet head */}
      <div className="flex items-center justify-between px-4 h-11 border-b border-line shrink-0">
        <div className="flex items-center gap-1.5 mono text-xs text-fg-3 truncate">
          <span>Library</span>
          <span className="text-fg-4">▸</span>
          <span className="text-fg-2 truncate" title={displayAlbum.title ?? displayAlbum.dir_name}>
            {displayAlbum.title || displayAlbum.dir_name}
          </span>
        </div>
        <Dialog.Close asChild>
          <button
            className="w-8 h-8 grid place-items-center text-fg-3 hover:text-fg hover:bg-bg-3 rounded transition"
            aria-label="Close"
          >
            <X className="w-4 h-4" />
          </button>
        </Dialog.Close>
      </div>

      {/* Sheet body — scrolls */}
      <div className="flex-1 overflow-y-auto">
        <AlbumHero album={displayAlbum} />

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
                disabled={retryJob.isPending}
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
            applying={isApplying}
          />
        ) : (
          <>
            <Section id="album-info" title="Album info">
              <PropTable album={displayAlbum} />
            </Section>

            {/* Findings (auto-hidden when this album has none open) */}
            <FindingsSection albumId={album.id} />

            {/* My Tags — user's own taxonomy ("Cocktail Hour", "Christmas",
                "Instrumental"…) for shuffling and playlist construction.
                Distinct from FLAC GENRE; stored per-track in the cache + a
                hidden sidecar JSON in the album dir. */}
            <MyTagsSection albumId={album.id} tracks={tracks.data?.tracks ?? []} />

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
                <AudioAnalysis tracks={tracks.data.tracks} />
              ) : (
                <div className="px-4 py-3 text-xs text-fg-4">Loading…</div>
              )}
            </Section>

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
                />
              ) : (
                <div className="px-4 py-3 text-xs text-fg-4">Loading tracks…</div>
              )}
            </Section>
          </>
        )}
      </div>

      {/* Hint dialog — gates each AI call. We treat both "cover" AND
          "metadata+cover" combos as cover-mode for prior-attempts purposes,
          since either kind will run a cover lookup. */}
      <HintDialog
        open={pendingType !== null}
        mode={pendingType === "metadata" ? "metadata" : "cover"}
        albumTitle={displayAlbum.title || displayAlbum.dir_name}
        priorCoverAttempts={
          pendingType === "cover" || pendingType === "metadata+cover"
            ? priorCoverAttempts
            : []
        }
        onSubmit={onHintSubmit}
        onCancel={onHintCancel}
      />

      {/* Sheet footer */}
      <div className="flex items-center gap-2 h-12 px-3 border-t border-line bg-panel-2 shrink-0">
        {isApplying ? (
          <span className="text-xs text-fg-3 mono">Writing — keep this panel open</span>
        ) : (
          <>
            {/* Buttons disabled while a job exists for this album (queued,
                running, or completed-not-yet-acted). One job per album. */}
            <button
              onClick={onSuggestCover}
              disabled={!!activeJob}
              className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
              title={activeJob ? "An AI job already exists for this album — view it above" : undefined}
            >
              <ImageIcon className="w-3.5 h-3.5 shrink-0" />
              <span>Find cover art</span>
            </button>
            <button
              onClick={onSuggestMetadata}
              disabled={!!activeJob}
              className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5 whitespace-nowrap shrink-0"
              title={
                activeJob
                  ? "An AI job already exists for this album — view it above"
                  : !albumHasCover
                  ? "No cover on disk — will also search for cover art"
                  : undefined
              }
            >
              <Sparkles className="w-3.5 h-3.5 shrink-0" />
              <span>Suggest fixes{!albumHasCover && !activeJob ? " (+ cover)" : ""}</span>
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
