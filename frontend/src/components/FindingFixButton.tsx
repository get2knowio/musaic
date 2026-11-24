/**
 * Fix-button for a health-check finding. Visually reflects the lifecycle
 * of the research request that's handling the fix, so the user doesn't need a toast
 * to know what just happened.
 *
 * State machine (driven by the active job for this finding's album, if any
 * AND if that job's type would actually resolve THIS finding):
 *
 *   no matching active job → "Fix"      (accent, clickable → enqueues a job)
 *   job queued             → "Queued"   (muted, disabled, clock icon)
 *   job running            → "Running"  (accent, disabled, spinner)
 *   job completed unacted  → "Ready"    (ok-toned, clickable → opens album sheet)
 *
 * Vocabulary kept in lockstep with JobStatusBadge (Library tiles / list rows)
 * and the Jobs page status pill — the user navigates between those surfaces
 * during a normal session and parity reduces cognitive load.
 *
 * Why we filter by job_type, not just album: an album can have multiple
 * findings of different kinds (no-cover-art AND unidentified-album AND
 * tags-out-of-sync). Each finding is fixed by a *specific* job type — a
 * cover job doesn't help the identify finding. Without filtering, every
 * Fix click would flip every other finding's button on the same album to
 * "Running" too, which is confusing and wrong.
 *
 * Deterministic checks (duplicate-tracks, inconsistent-artist-sort,
 * filename-mismatch) never make a research request — their fix path is
 * synchronous. They get an empty job-type allowlist so the button stays
 * in idle "Fix" state even when an unrelated request is running on the
 * same album. The four research-capable checks try the resolver first and
 * send what it cannot settle to the assistant as a research request —
 * they DO appear in JOB_TYPES_FOR_CHECK and HINT_MODE_FOR_CHECK below.
 *
 * Errors (rare — usually a 409 conflict from a race) still toast, since the
 * button state didn't change as expected and the user needs the explanation.
 */
import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { CheckCircle2, Clock, FileEdit, Files, Layers, Loader2, Users, Zap } from "lucide-react";
import { toast } from "sonner";

import { useActiveJobForAlbum, usePriorCoverAttempts } from "@/hooks/useJobs";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useDelegatedWork } from "@/hooks/useDelegatedWork";
import { useFixFinding } from "@/hooks/useChecks";
import { batchedActiveJobs } from "@/lib/batchedResearch";
import { fixActionCopy } from "@/lib/findingActions";
import type { CheckFixKind, Finding, Job, JobType } from "@/lib/types";

import ArtistSortResolveDialog from "./ArtistSortResolveDialog";
import DuplicateResolveDialog from "./DuplicateResolveDialog";
import FilenameMismatchResolveDialog from "./FilenameMismatchResolveDialog";
import MultiDiscResolveDialog from "./MultiDiscResolveDialog";
import HintDialog from "./DetailSheet/HintDialog";


/** Which research job_types would resolve a finding from each check. A
 *  finding's button only reflects the album's active request when its type
 *  is in this list — otherwise it stays idle. Checks not listed
 *  (duplicate-tracks, inconsistent-artist-sort, filename-mismatch) never
 *  make a research request, so the lookup falls through to "idle." */
const JOB_TYPES_FOR_CHECK: Record<string, JobType[]> = {
  "no-cover-art":        ["cover", "metadata+cover"],
  "track-gaps":          ["metadata", "metadata+cover"],
  "unidentified-album":  ["metadata", "metadata+cover"],
  "tags-out-of-sync":    ["metadata", "metadata+cover"],
};

/** Which HintDialog mode to use for each research-capable check. The mode
 *  picks the dialog's title, placeholder, and whether prior-cover-attempts
 *  get shown. Checks not listed don't open the hint dialog (they're
 *  deterministic). */
const HINT_MODE_FOR_CHECK: Record<string, "metadata" | "cover"> = {
  "no-cover-art":        "cover",
  "track-gaps":          "metadata",
  "unidentified-album":  "metadata",
  "tags-out-of-sync":    "metadata",
};

interface Props {
  finding: Finding;
  fixKind: CheckFixKind;
  /** Sibling findings from the same list view, in display order.
   *  Only the cluster-resolve dialog uses this today — it lets the
   *  user chain through resolutions without bouncing back to the
   *  list page after each apply. Optional so single-row callers
   *  (e.g. the album DetailSheet's FindingsSection) don't have to
   *  thread it through if they don't care. */
  siblings?: Finding[];
}

export default function FindingFixButton({ finding, fixKind, siblings }: Props) {
  const navigate = useNavigate();
  const fixMutation = useFixFinding();
  // The active job on this album, regardless of type. We narrow it below
  // by job_type before treating it as "this finding's job" — see the
  // matchingJob computation.
  const albumJob = useActiveJobForAlbum(finding.album_id);
  const [duplicateOpen, setDuplicateOpen] = useState(false);
  const [artistSortOpen, setArtistSortOpen] = useState(false);
  const [filenameOpen, setFilenameOpen] = useState(false);
  const [multiDiscOpen, setMultiDiscOpen] = useState(false);
  const [hintOpen, setHintOpen] = useState(false);

  // Album lookup for the hint-dialog header — keeps the title in the
  // dialog matching what the user just clicked Fix on. Falls back through
  // (title || dir_name || album_id) so we always show *something*.
  const { data: albums = [] } = useAlbumsSnapshot();
  const album = finding.album_id ? albums.find((a) => a.id === finding.album_id) ?? null : null;
  const albumTitle = album?.title || album?.dir_name || finding.album_id || "";

  // Prior cover-lookup attempts surface in the dialog's warning banner
  // when mode === "cover", so the user sees at confirmation time if
  // they're about to retry something that already failed N times.
  const priorCoverAttempts = usePriorCoverAttempts(finding.album_id);

  // Only treat the album's active job as relevant if its type would
  // actually resolve THIS finding. Checks with no entry in the table
  // (deterministic ones like duplicate-tracks, inconsistent-artist-sort,
  // filename-mismatch) get null here unconditionally so an unrelated
  // request on the same album doesn't drag their button into "Running."
  const allowedTypes = JOB_TYPES_FOR_CHECK[finding.check_id];
  // A request a Fix all run made has no job card (the jobs list leaves
  // batched cards out), so the album's batched request comes from the
  // delegated work list; without it the button would offer Fix again while
  // research is already pending.
  const { data: work } = useDelegatedWork();
  const batchedJob = useMemo(
    () =>
      finding.album_id
        ? batchedActiveJobs(
            (Array.isArray(work) ? work : []).filter((w) => w.albumId === finding.album_id),
          ).get(finding.album_id) ?? null
        : null,
    [work, finding.album_id],
  );
  const relevant = (j: Job | null) => (j && allowedTypes && allowedTypes.includes(j.job_type) ? j : null);
  const activeJob = relevant(albumJob) ?? relevant(batchedJob);

  // The research-capable checks always offer a hint first; otherwise the
  // click goes straight through to the mutation with no hint.
  const hintMode = HINT_MODE_FOR_CHECK[finding.check_id] ?? null;
  // The words on the button (lib/findingActions.ts), the same in the inbox
  // and the album inspector.
  const copy = fixActionCopy(finding.check_id);

  // Multi-disc-layout: one Resolve button for every finding, as for the
  // other dialog checks. Whether this set can convert (one folder, nothing
  // in the way) is the dialog's re-derived answer, not the finding's, so
  // the button does not guess it; the dialog says plainly when it cannot.
  if (finding.check_id === "multi-disc-layout" && finding.album_id != null && !finding.ignored_at) {
    return (
      <>
        <button
          onClick={(e) => { e.stopPropagation(); setMultiDiscOpen(true); }}
          className="h-7 px-2 rounded border border-line text-xs text-fg-2 hover:bg-bg-3 transition inline-flex items-center gap-1"
          title={copy.title}
        >
          <Layers className="w-3 h-3" />
          {copy.label}
        </button>
        <MultiDiscResolveDialog
          finding={finding}
          open={multiDiscOpen}
          onClose={() => setMultiDiscOpen(false)}
        />
      </>
    );
  }

  // Library-level findings (no album_id) and findings on un-fixable checks
  // get no button at all.
  if (fixKind === "none" || finding.album_id == null || finding.ignored_at) {
    return null;
  }

  // Filename-mismatch uses a custom Resolve dialog showing the
  // current → expected diff for each track, since the rename is
  // best confirmed wholesale (and the user might want to skip
  // an album where the template's wrong before agreeing).
  if (finding.check_id === "filename-mismatch") {
    return (
      <>
        <button
          onClick={(e) => { e.stopPropagation(); setFilenameOpen(true); }}
          className="h-7 px-2 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition inline-flex items-center gap-1"
          title={copy.title}
        >
          <FileEdit className="w-3 h-3" />
          {copy.label}
        </button>
        <FilenameMismatchResolveDialog
          finding={finding}
          findings={siblings}
          open={filenameOpen}
          onClose={() => setFilenameOpen(false)}
        />
      </>
    );
  }

  // Inconsistent-artist-sort uses a cluster-level Resolve dialog —
  // the user picks ONE canonical sort value to bulk-apply across
  // every album in the cluster.
  if (finding.check_id === "inconsistent-artist-sort") {
    return (
      <>
        <button
          onClick={(e) => { e.stopPropagation(); setArtistSortOpen(true); }}
          className="h-7 px-2 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition inline-flex items-center gap-1"
          title={copy.title}
        >
          <Users className="w-3 h-3" />
          {copy.label}
        </button>
        <ArtistSortResolveDialog
          finding={finding}
          findings={siblings}
          open={artistSortOpen}
          onClose={() => setArtistSortOpen(false)}
        />
      </>
    );
  }

  // Duplicate-tracks check has a custom Resolve dialog rather than a
  // single-click fix — the user has to pick which copy to keep.
  if (finding.check_id === "duplicate-tracks") {
    return (
      <>
        <button
          onClick={(e) => { e.stopPropagation(); setDuplicateOpen(true); }}
          className="h-7 px-2 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition inline-flex items-center gap-1"
          title={copy.title}
        >
          <Files className="w-3 h-3" />
          {copy.label}
        </button>
        <DuplicateResolveDialog
          finding={finding}
          open={duplicateOpen}
          onClose={() => setDuplicateOpen(false)}
        />
      </>
    );
  }

  // ---- Active job in flight: render the matching state -----------------
  if (activeJob) {
    if (activeJob.status === "queued") {
      return (
        <button
          disabled
          className="h-7 px-2 rounded border border-line text-xs text-fg-3 inline-flex items-center gap-1 cursor-not-allowed"
          title="Research is pending for this album"
        >
          <Clock className="w-3 h-3" />
          Queued
        </button>
      );
    }
    if (activeJob.status === "running") {
      return (
        <button
          disabled
          className="h-7 px-2 rounded bg-accent-soft text-accent text-xs font-medium inline-flex items-center gap-1 cursor-not-allowed"
          title="An assistant is working on this"
        >
          <Loader2 className="w-3 h-3 spin" />
          Running
        </button>
      );
    }
    if (activeJob.status === "completed") {
      // Research finished, suggestion is waiting for the user to Apply or Discard.
      // Send them to the album sheet, which already surfaces the result.
      return (
        <button
          onClick={(e) => {
            e.stopPropagation();
            // Defer past Sonner's click handler / Radix focus trap (same
            // pattern useJobsStream uses for its toast actions).
            const path = `/library/${encodeURIComponent(finding.album_id!)}`;
            setTimeout(() => navigate(path), 0);
          }}
          className="h-7 px-2 rounded bg-ok text-white text-xs font-medium hover:opacity-90 transition inline-flex items-center gap-1"
          title="Suggestion ready — review and apply"
        >
          <CheckCircle2 className="w-3 h-3" />
          Ready
        </button>
      );
    }
    // failed / acted_on — fall through to the idle "Fix" state below so the
    // user can retry or trigger a new lookup.
  }

  // ---- Idle: clickable Fix button --------------------------------------
  const submitFix = (hint: string) => {
    fixMutation.mutate(
      { checkId: finding.check_id, findingId: finding.id, hint },
      {
        onSuccess: (r) => {
          if (!r.ok) {
            toast.error(r.message || "Fix didn't apply");
            return;
          }
          // Happy path:
          //   - r.job_id null → MB-direct (or other deterministic) fix
          //     succeeded synchronously. SSE won't queue a job, so without
          //     a toast the user gets no feedback at all beyond the row
          //     eventually disappearing on the next check refresh.
          //   - r.job_id set  → research requested. The SSE `state` event
          //     will flip the active job in our cache and the button rerenders
          //     to "Queued" automatically; no toast needed.
          if (r.job_id == null) {
            toast.success(r.message || "Fix applied", { duration: 3000 });
          }
        },
        onError: (err) => {
          const e2 = err as Error & { status?: number };
          toast.error(e2.message || "Couldn't queue the fix");
        },
      },
    );
  };

  const onFix = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (hintMode) {
      // Research-capable check: open the hint dialog first so the user
      // gets the same opportunity to add context as the album panel's
      // Request research does. Submitting the dialog (even with empty hint) fires the fix
      // mutation; canceling does nothing.
      setHintOpen(true);
      return;
    }
    // Deterministic check with no hint mode. Fire straight through with
    // no dialog.
    submitFix("");
  };

  return (
    <>
      <button
        onClick={onFix}
        disabled={fixMutation.isPending}
        className="h-7 px-2 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1"
        title={copy.title}
      >
        <Zap className="w-3 h-3" />
        {copy.label}
      </button>
      {hintMode && (
        <HintDialog
          open={hintOpen}
          mode={hintMode}
          albumTitle={albumTitle}
          checkId={finding.check_id}
          priorCoverAttempts={hintMode === "cover" ? priorCoverAttempts : []}
          onSubmit={(hint, _attachCover) => {
            setHintOpen(false);
            submitFix(hint);
          }}
          onCancel={() => setHintOpen(false)}
        />
      )}
    </>
  );
}
