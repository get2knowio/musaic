/**
 * Fix-button for a health-check finding. Visually reflects the lifecycle
 * of the AI job that's handling the fix, so the user doesn't need a toast
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
 * filename-mismatch) don't enqueue AI jobs at all — their fix path is
 * synchronous. They get an empty job-type allowlist so the button stays
 * in idle "Fix" state even when an unrelated AI job is running on the
 * same album. The hybrid checks (`unidentified-album`, `tags-out-of-sync`)
 * try MB-direct first and fall through to an AI metadata job when MB
 * can't disambiguate — they DO appear in JOB_TYPES_FOR_CHECK and
 * HINT_MODE_FOR_CHECK below.
 *
 * Errors (rare — usually a 409 conflict from a race) still toast, since the
 * button state didn't change as expected and the user needs the explanation.
 */
import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { CheckCircle2, Clock, FileEdit, Files, Loader2, Users, Zap } from "lucide-react";
import { toast } from "sonner";

import { useActiveJobForAlbum, usePriorCoverAttempts } from "@/hooks/useJobs";
import { useAlbumsSnapshot } from "@/hooks/useAlbumsSnapshot";
import { useFixFinding } from "@/hooks/useChecks";
import { useAiStatus } from "@/hooks/useAiStatus";
import { fixIsMusicBrainzOnly, offersHintEntry } from "@/lib/aiGating";
import type { CheckFixKind, Finding, JobType } from "@/lib/types";

import ArtistSortResolveDialog from "./ArtistSortResolveDialog";
import DuplicateResolveDialog from "./DuplicateResolveDialog";
import FilenameMismatchResolveDialog from "./FilenameMismatchResolveDialog";
import HintDialog from "./DetailSheet/HintDialog";


/** Which AI job_types would resolve a finding from each check. A finding's
 *  button only reflects the album's active job when that job's type is in
 *  this list — otherwise it stays idle. Checks not listed
 *  (duplicate-tracks, inconsistent-artist-sort, filename-mismatch) don't
 *  go through the AI queue, so the lookup falls through to "idle."
 *
 *  Both consolidated checks (`unidentified-album` and `tags-out-of-sync`)
 *  are here because per-finding Fix can fall through to AI: the identify
 *  cascade ends with an AI metadata job when the resolver can't anchor;
 *  the tag-sync fix falls through to AI when the resolver can't fill all
 *  the missing fields (genre, or anything on an unanchored album). */
const JOB_TYPES_FOR_CHECK: Record<string, JobType[]> = {
  "no-cover-art":        ["cover", "metadata+cover"],
  "track-gaps":          ["metadata", "metadata+cover"],
  "unidentified-album":  ["metadata", "metadata+cover"],
  "tags-out-of-sync":    ["metadata", "metadata+cover"],
};

/** Which HintDialog mode to use for each AI-job check. The mode picks the
 *  dialog's title, placeholder, and whether prior-cover-attempts get shown.
 *  Checks not listed don't open the hint dialog (they're either deterministic
 *  or have no AI path). */
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
  const [hintOpen, setHintOpen] = useState(false);
  // Without a model backend an ai-job fix runs its MusicBrainz part only;
  // where it would need a model the server answers ok:false with the
  // not-available message, shown as the error toast
  // (lib/aiGating.ts). No hint dialog: there is no model to steer.
  const ai = useAiStatus();
  const mbOnly = fixIsMusicBrainzOnly(fixKind, ai);

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
  // filename-mismatch) get null here unconditionally so an unrelated AI
  // job on the same album doesn't drag their button into "Running."
  // The hybrid checks (`unidentified-album`, `tags-out-of-sync`) ARE in
  // the table — an active metadata job correctly reflects to their button.
  const allowedTypes = JOB_TYPES_FOR_CHECK[finding.check_id];
  const activeJob =
    albumJob && allowedTypes && allowedTypes.includes(albumJob.job_type)
      ? albumJob
      : null;

  // For ai-job checks we have a hint mode; otherwise the click goes
  // straight through to the mutation with no hint.
  const hintMode = offersHintEntry(ai) ? HINT_MODE_FOR_CHECK[finding.check_id] ?? null : null;

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
          title="Review the new file names, then rename the files"
        >
          <FileEdit className="w-3 h-3" />
          Resolve…
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
          title="Give this artist the same sort name everywhere"
        >
          <Users className="w-3 h-3" />
          Resolve…
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
          title="Choose which copies to move to the trash"
        >
          <Files className="w-3 h-3" />
          Resolve…
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
          title={ai.ready ? "An AI job is queued for this album" : "A job is queued for this album"}
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
          title={ai.ready ? "Claude is working on this" : "A job is working on this"}
        >
          <Loader2 className="w-3 h-3 spin" />
          Running
        </button>
      );
    }
    if (activeJob.status === "completed") {
      // Job finished, suggestion is waiting for the user to Apply or Discard.
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
          //   - r.job_id set  → AI job queued. The SSE `state` event will
          //     flip the active job in our cache and the button rerenders
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
      // AI-job check: open the hint dialog first so the user gets the
      // same opportunity to add context as the detail-sheet AI buttons
      // do. Submitting the dialog (even with empty hint) fires the fix
      // mutation; canceling does nothing.
      setHintOpen(true);
      return;
    }
    // Deterministic check with no hint mode. (Hybrid checks like
    // unidentified-album and tags-out-of-sync DO have a hint mode and
    // take the branch above.) Fire straight through with no dialog.
    submitFix("");
  };

  return (
    <>
      <button
        onClick={onFix}
        disabled={fixMutation.isPending}
        className="h-7 px-2 rounded bg-accent text-accent-fg text-xs font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1"
        title={
          mbOnly
            ? "Fix from MusicBrainz data. A finding MusicBrainz can't settle stays open."
            : hintMode
              ? "Add a hint and queue an AI fix for this finding"
              : "Queue a fix for this finding"
        }
      >
        <Zap className="w-3 h-3" />
        Fix
      </button>
      {hintMode && (
        <HintDialog
          open={hintOpen}
          mode={hintMode}
          albumTitle={albumTitle}
          checkId={finding.check_id}
          priorCoverAttempts={hintMode === "cover" ? priorCoverAttempts : []}
          // No `hasCover` passed → checkbox stays hidden in this
          // flow. The fix-finding endpoint doesn't expose
          // attach_cover today (would need plumbing through
          // checks_runner.fix_finding → Check.fix() → each check's
          // enqueue call). The DetailSheet's footer "Suggest
          // fixes" flow already lets the user A/B test; that's
          // where they should toggle. If we want toggle support
          // here too, plumb attach_cover through the fix-finding
          // path in a follow-up.
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
