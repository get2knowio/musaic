/**
 * Per-finding resolve dialog for the inconsistent-artist-sort check.
 *
 * Shows the cluster of credit instances whose primary artist matches
 * but whose sort tags don't agree, lets the user pick the canonical
 * sort form, and bulk-writes it to:
 *   - ALBUMARTISTSORT for album-kind members (every FLAC in the album)
 *   - ARTISTSORT for track-kind members (the listed track filenames only)
 *
 * Track-kind members are most common on compilations: a "Best of Easy
 * Listening" disc with four tracks credited to four different
 * "Ray Conniff" credit variations. Resolving the cluster sweeps those
 * tracks AND any solo Conniff albums onto the same sort string.
 *
 * Three input modes depending on the cluster's current state (carried
 * in `finding.detail.state`):
 *
 *   propagatable  — one cluster member already has a canonical sort.
 *                   Pre-fill the input with that value; user clicks
 *                   Apply to spread it.
 *   conflicting   — multiple distinct canonical values across the
 *                   cluster. Surface them as quick-pick chips; user
 *                   either taps one or types their own.
 *   unsorted      — no member has a sort yet. Empty input, user types.
 *                   Auto-suggest article rotation for the first credit
 *                   when applicable (deterministic, never wrong).
 */
import { useEffect, useMemo, useRef, useState } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, Disc3, FileAudio, Loader2, Users, X } from "lucide-react";
import { toast } from "sonner";

import { applyInconsistentArtistSort } from "@/lib/api";
import { CHECKS_QUERY_KEY, CHECK_DETAIL_KEY, FINDINGS_QUERY_KEY } from "@/hooks/useChecks";
import type { Finding } from "@/lib/types";

/**
 * v11+ cluster member shape. The `kind` discriminator distinguishes
 * album-level credits (album_artist) from track-level ones
 * (per-track artist). Album members reference the album as a whole;
 * track members carry a list of filenames within one album that
 * share the credit.
 *
 * The `sort` field holds the current ALBUMARTISTSORT (for album
 * members) or ARTISTSORT (for track members) — empty string when
 * no sort tag is set.
 */
type ClusterMember =
  | {
      kind: "album";
      album_id: string;
      dir_name?: string;
      album_artist?: string;
      sort?: string;
    }
  | {
      kind: "track";
      album_id: string;
      dir_name?: string;
      filenames?: string[];
      artist?: string;
      sort?: string;
    };

/** Legacy (v10) member shape — fields named "id" / "album_artist_sort".
 *  Existing findings stored before v11 use this shape; we coerce on
 *  read so the dialog can still render them until the next check rerun
 *  rebuilds the finding. */
interface LegacyMember {
  id?: string;
  album_id?: string;
  dir_name?: string;
  album_artist?: string;
  album_artist_sort?: string | null;
  kind?: never;
}

interface ClusterDetail {
  primary_token?: string;
  state?: "propagatable" | "unsorted" | "conflicting";
  canonical_sort?: string | null;
  members?: (ClusterMember | LegacyMember)[];
}

interface Props {
  finding: Finding;
  open: boolean;
  onClose: () => void;
  /** Optional list of findings to chain through after each apply.
   *  When provided, applying advances to the next eligible finding
   *  (open, not ignored, same check) instead of closing the dialog —
   *  saving the user a round-trip back to the list page when working
   *  through a long backlog. The list is snapshotted on dialog open
   *  so post-apply cache invalidation doesn't shift positions
   *  underneath us. Falls back to single-finding mode when omitted. */
  findings?: Finding[];
}

export default function ArtistSortResolveDialog({
  finding,
  open,
  onClose,
  findings,
}: Props) {
  // Snapshot the chain when the dialog first opens. Subsequent
  // applies invalidate the underlying findings cache, which can
  // shrink the list of "open" findings — but we want to keep
  // walking the originally-displayed sequence so the user's
  // mental model ("there were 304, I was on 17, now I'm on 18")
  // stays consistent with what they see.
  const [chainSnapshot, setChainSnapshot] = useState<Finding[]>([]);
  const [currentFindingId, setCurrentFindingId] = useState<number>(finding.id);

  useEffect(() => {
    if (!open) return;
    // Build the chain when the dialog opens. Filter to the same
    // check, drop already-resolved / ignored entries, and dedupe.
    const filtered = (findings ?? [finding]).filter(
      (f) =>
        f.check_id === finding.check_id
        && !f.resolved_at
        && !f.ignored_at,
    );
    setChainSnapshot(filtered.length > 0 ? filtered : [finding]);
    setCurrentFindingId(finding.id);
  }, [open, finding.id]);   // eslint-disable-line react-hooks/exhaustive-deps

  // The finding currently being displayed. Falls back to the prop
  // when the chain doesn't contain the current id (e.g. legacy
  // single-finding callers, or after we've advanced past the end).
  const currentFinding =
    chainSnapshot.find((f) => f.id === currentFindingId) ?? finding;
  const currentIndex = chainSnapshot.findIndex(
    (f) => f.id === currentFindingId,
  );
  const totalInChain = chainSnapshot.length;
  const hasNext = currentIndex >= 0 && currentIndex < totalInChain - 1;

  const detail = (currentFinding.detail || {}) as ClusterDetail;
  const rawMembers = detail.members ?? [];
  const members: ClusterMember[] = useMemo(
    () => rawMembers.map(_normalize).filter((m): m is ClusterMember => m !== null),
    [rawMembers],
  );
  const state = detail.state ?? "unsorted";
  const canonical = detail.canonical_sort ?? null;
  const qc = useQueryClient();

  const albumCount = useMemo(
    () => new Set(members.filter((m) => m.kind === "album").map((m) => m.album_id)).size,
    [members],
  );
  const trackCount = useMemo(
    () =>
      members
        .filter((m): m is Extract<ClusterMember, { kind: "track" }> => m.kind === "track")
        .reduce((n, m) => n + (m.filenames?.length ?? 0), 0),
    [members],
  );

  // Pre-existing sort values across the cluster — used for
  // conflicting-state quick-picks.
  const existingSorts = useMemo<string[]>(() => {
    const set = new Set<string>();
    for (const m of members) {
      const s = (m.sort ?? "").trim();
      if (s) set.add(s);
    }
    return Array.from(set).sort();
  }, [members]);

  // Initial input value: prefer the canonical the backend identified
  // (propagatable case), then any pre-existing sort if there's only
  // one, then auto-suggest article rotation off the first credit.
  const seedValue = useMemo(() => {
    if (canonical) return canonical;
    if (existingSorts.length === 1) return existingSorts[0];
    const firstCredit = _creditOf(members[0]);
    return _autoSuggestSort(firstCredit);
  }, [canonical, existingSorts, members]);

  const [sortValue, setSortValue] = useState(seedValue);
  // Reset the input when the displayed finding changes (initial open
  // OR chain advancement). Auto-focus is handled below via a ref so
  // each advancement re-focuses without manual clicking.
  useEffect(() => {
    if (open) setSortValue(seedValue);
  }, [open, currentFindingId, seedValue]);

  // Hold a ref to the canonical sort input so we can imperatively
  // focus it on every advancement, not just initial open. autoFocus
  // alone fires only on first mount; the dialog stays mounted across
  // chain advances so we re-focus by hand here.
  const sortInputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!open) return;
    // Defer to next paint so the input is in the DOM before focus.
    const handle = window.requestAnimationFrame(() => {
      sortInputRef.current?.focus();
      sortInputRef.current?.select();
    });
    return () => window.cancelAnimationFrame(handle);
  }, [open, currentFindingId]);

  const mutation = useMutation({
    mutationFn: (v: string) =>
      applyInconsistentArtistSort(currentFinding.id, v),
    onSuccess: (r) => {
      toast.success(r.message);
      qc.invalidateQueries({ queryKey: FINDINGS_QUERY_KEY });
      qc.invalidateQueries({ queryKey: CHECKS_QUERY_KEY });
      // The Health detail page renders from the per-check detail
      // cache (CHECK_DETAIL_KEY), not the global findings list.
      // Without this the resolved cluster keeps showing on the page
      // even though server-side it was auto-resolved by the post-
      // apply check rerun.
      qc.invalidateQueries({
        queryKey: CHECK_DETAIL_KEY("inconsistent-artist-sort"),
      });
      // Advance through the chain instead of closing on every apply.
      // Saves the user a round-trip back to the list page when
      // they're working through dozens / hundreds of findings.
      if (hasNext) {
        const nextFinding = chainSnapshot[currentIndex + 1];
        setCurrentFindingId(nextFinding.id);
      } else {
        toast.success(
          totalInChain > 1
            ? `Done — resolved all ${totalInChain} clusters in this batch.`
            : "Done.",
          { duration: 3000 },
        );
        onClose();
      }
    },
    onError: (e) => {
      toast.error((e as Error).message || "Couldn't apply sort value");
    },
  });

  const onApply = () => {
    const v = sortValue.trim();
    if (!v) return;
    mutation.mutate(v);
  };

  const onSkipNext = () => {
    if (hasNext) {
      const nextFinding = chainSnapshot[currentIndex + 1];
      setCurrentFindingId(nextFinding.id);
    } else {
      onClose();
    }
  };

  const applyButtonLabel = useMemo(() => {
    const parts: string[] = [];
    if (albumCount > 0) parts.push(`${albumCount} album${albumCount === 1 ? "" : "s"}`);
    if (trackCount > 0) parts.push(`${trackCount} track${trackCount === 1 ? "" : "s"}`);
    return `Apply to ${parts.join(" + ") || "cluster"}`;
  }, [albumCount, trackCount]);

  return (
    <Dialog.Root open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-40 bg-black/45 backdrop-blur-[1px]" />
        <Dialog.Content
          className="fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 w-[min(680px,95vw)] bg-panel border border-line rounded-md shadow-sheet outline-none flex flex-col"
          aria-describedby={undefined}
        >
          <div className="flex items-center justify-between px-4 h-11 border-b border-line">
            <div className="flex items-center gap-2 min-w-0">
              <Users className="w-3.5 h-3.5 text-accent shrink-0" />
              <Dialog.Title className="font-semibold text-md truncate">
                Normalize artist sort
              </Dialog.Title>
              {/* Chain progress indicator. Only shown when there's
                  more than one finding in the snapshot, so single-
                  resolve callers don't see a confusing "1 of 1". */}
              {totalInChain > 1 && currentIndex >= 0 && (
                <span className="mono text-[11px] text-fg-4 tabular-nums shrink-0">
                  · {currentIndex + 1} of {totalInChain}
                </span>
              )}
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

          <div className="px-4 py-3.5 space-y-3 overflow-y-auto max-h-[70vh]">
            <p className="text-sm text-fg-2">
              {_summarize(albumCount, trackCount)} use credit variations that
              all refer to the same artist. Setting a single canonical sort
              value (typically <code className="mono text-fg">"Last, First"</code>)
              groups them together in Plex / Roon sidebars and track-artist
              views.
            </p>

            {state === "propagatable" && canonical && (
              <div className="flex items-start gap-2 text-xs text-fg-2 leading-relaxed bg-ok-soft border border-ok/30 rounded px-3 py-2">
                <CheckCircle2 className="w-3.5 h-3.5 text-ok shrink-0 mt-0.5" />
                <span>
                  One member of this cluster already has{" "}
                  <code className="mono text-fg">{canonical}</code> — applying
                  it to the rest will normalize the cluster.
                </span>
              </div>
            )}

            {state === "conflicting" && (
              <div className="text-xs text-fg-2 leading-relaxed bg-warn-soft border border-warn/30 rounded px-3 py-2">
                Members of this cluster have <b>different</b> sort tags. Pick
                the one that should win, or type a new value.
                <div className="flex flex-wrap gap-1.5 mt-2">
                  {existingSorts.map((s) => (
                    <button
                      key={s}
                      onClick={() => setSortValue(s)}
                      className={`mono text-[11px] px-2 py-0.5 rounded-full border transition ${
                        sortValue === s
                          ? "border-accent bg-accent-soft text-accent"
                          : "border-line bg-panel hover:bg-bg-3 text-fg-2"
                      }`}
                    >
                      {s}
                    </button>
                  ))}
                </div>
              </div>
            )}

            <div>
              <label className="mono text-xs uppercase tracking-[0.06em] text-fg-4 block mb-1.5">
                Canonical sort value
              </label>
              <input
                ref={sortInputRef}
                type="text"
                value={sortValue}
                onChange={(e) => setSortValue(e.target.value)}
                placeholder="e.g. Conniff, Ray"
                disabled={mutation.isPending}
                onKeyDown={(e) => { if (e.key === "Enter") onApply(); }}
                className="w-full bg-bg border border-line rounded-md px-3 py-2 text-sm focus:outline-none focus:border-accent focus:ring-2 focus:ring-accent/30 disabled:opacity-60"
                maxLength={200}
                autoFocus
              />
              <div className="text-[11px] text-fg-4 mt-1 leading-relaxed">
                Album members get ALBUMARTISTSORT; track members get
                ARTISTSORT on the listed filenames only. Display
                tags (album_artist / artist) stay untouched.
              </div>
            </div>

            <div className="border border-line rounded-md bg-panel-2 divide-y divide-line">
              <div className="flex items-center px-3 py-1.5 border-b border-line text-[11px] mono uppercase tracking-[0.06em] text-fg-4">
                <span>Cluster ({members.length} {members.length === 1 ? "member" : "members"})</span>
              </div>
              {members.map((m, i) => (
                <ClusterRow
                  key={`${m.album_id}-${m.kind}-${i}`}
                  member={m}
                  pendingSort={sortValue.trim()}
                />
              ))}
            </div>
          </div>

          <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-line bg-panel-2 rounded-b-md">
            <Dialog.Close asChild>
              <button
                className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
                disabled={mutation.isPending}
              >
                Cancel
              </button>
            </Dialog.Close>
            {/* Skip → next: walk past this cluster without writing
                anything (e.g. "I'm not sure about this artist, come
                back to it later"). Only meaningful when the chain
                has more entries to advance to. */}
            {hasNext && (
              <button
                onClick={onSkipNext}
                disabled={mutation.isPending}
                className="h-8 px-3 border border-line rounded text-sm text-fg-2 hover:bg-bg-3 transition disabled:opacity-60"
                title="Don't change this one — go to the next finding"
              >
                Skip →
              </button>
            )}
            <button
              onClick={onApply}
              disabled={mutation.isPending || !sortValue.trim()}
              className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
            >
              {mutation.isPending ? (
                <><Loader2 className="w-3.5 h-3.5 spin" /> Applying…</>
              ) : hasNext ? (
                <>{applyButtonLabel} & next</>
              ) : (
                applyButtonLabel
              )}
            </button>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function ClusterRow({
  member,
  pendingSort,
}: {
  member: ClusterMember;
  /** Canonical sort value the user has typed in the input field —
   *  what every cluster member will be set to once Apply runs.
   *  Empty string means the user hasn't typed anything yet (in
   *  which case we just show the current state without a preview). */
  pendingSort: string;
}) {
  const isTrack = member.kind === "track";
  const credit = _creditOf(member);
  const currentSort = (member.sort ?? "").trim();
  const filenames =
    member.kind === "track" ? (member.filenames ?? []) : [];

  // What's actually going to happen to this row when Apply runs.
  // Three states:
  //   - "add"     : no current tag, will be set to pendingSort
  //   - "change"  : current tag differs from pendingSort, will be overwritten
  //   - "keep"    : current tag already equals pendingSort, no-op
  // When the user hasn't typed anything yet (pendingSort empty),
  // we just show the current state without a before/after preview.
  let action: "add" | "change" | "keep" | "preview-only";
  if (!pendingSort) {
    action = "preview-only";
  } else if (!currentSort) {
    action = "add";
  } else if (currentSort === pendingSort) {
    action = "keep";
  } else {
    action = "change";
  }

  return (
    <div className="px-3 py-2 flex items-start gap-3 text-sm min-w-0">
      {isTrack ? (
        <FileAudio className="w-4 h-4 text-accent shrink-0 mt-0.5" />
      ) : (
        <Disc3 className="w-4 h-4 text-fg-4 shrink-0 mt-0.5" />
      )}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-1.5 min-w-0">
          {isTrack && (
            <span className="mono text-[10px] uppercase tracking-[0.08em] text-accent bg-accent/10 border border-accent/30 px-1.5 py-0.5 rounded shrink-0">
              Track
            </span>
          )}
          <span className="text-fg truncate" title={credit}>{credit}</span>
        </div>
        <div className="mono text-[11px] text-fg-4 truncate" title={member.dir_name}>
          {member.dir_name || member.album_id}
        </div>
        {isTrack && filenames.length > 0 && (
          <div className="mt-1 text-[11px] text-fg-3 leading-snug">
            {filenames.slice(0, 3).map((fn) => (
              <div key={fn} className="mono truncate" title={fn}>
                <span className="text-fg-4">·</span> {_prettifyFilename(fn)}
              </div>
            ))}
            {filenames.length > 3 && (
              <div
                className="text-fg-4 italic"
                title={filenames.slice(3).join("\n")}
              >
                + {filenames.length - 3} more
              </div>
            )}
          </div>
        )}
      </div>

      <div className="shrink-0 max-w-[45%] mt-0.5 text-right">
        <ChangePreview
          action={action}
          currentSort={currentSort}
          pendingSort={pendingSort}
        />
      </div>
    </div>
  );
}

/** Right-side "what will happen" indicator. Shows the current → new
 *  transition explicitly so the user can't misread the arrow as
 *  "this is what it will become" when it actually means "this is
 *  what's there now". */
function ChangePreview({
  action,
  currentSort,
  pendingSort,
}: {
  action: "add" | "change" | "keep" | "preview-only";
  currentSort: string;
  pendingSort: string;
}) {
  if (action === "preview-only") {
    // No canonical value typed yet — just show current state.
    return currentSort ? (
      <div
        className="mono text-[11px] text-fg-3 truncate"
        title={`current sort: ${currentSort}`}
      >
        currently {currentSort}
      </div>
    ) : (
      <div className="mono text-[11px] text-fg-4 italic">no sort tag</div>
    );
  }

  if (action === "keep") {
    return (
      <div
        className="mono text-[11px] text-fg-4 truncate"
        title={`already ${currentSort} — no change`}
      >
        no change
      </div>
    );
  }

  if (action === "add") {
    return (
      <div
        className="mono text-[11px] text-ok truncate"
        title={`will be set to ${pendingSort}`}
      >
        <span className="text-fg-4 italic">(none)</span>{" "}
        <span className="text-fg-4">→</span>{" "}
        <span className="text-ok">{pendingSort}</span>
      </div>
    );
  }

  // action === "change" — overwriting an existing value.
  return (
    <div className="mono text-[11px] truncate"
         title={`will overwrite ${currentSort} with ${pendingSort}`}>
      <span className="text-warn line-through">{currentSort}</span>{" "}
      <span className="text-fg-4">→</span>{" "}
      <span className="text-ok">{pendingSort}</span>
    </div>
  );
}

/** Display-shape for a track filename: drop the .flac extension so
 *  the row reads "01 - You Keep Me Hangin' On" instead of the noisier
 *  "01 - You Keep Me Hangin' On.flac". The track-position prefix and
 *  the song title are the parts that help the user identify which
 *  track is which inside a compilation. */
function _prettifyFilename(name: string): string {
  return name.replace(/\.flac$/i, "");
}

function _summarize(albums: number, tracks: number): string {
  const parts: string[] = [];
  if (albums > 0) parts.push(`${albums} album${albums === 1 ? "" : "s"}`);
  if (tracks > 0) parts.push(`${tracks} track${tracks === 1 ? "" : "s"}`);
  if (parts.length === 0) return "These items";
  return parts.join(" + ");
}

/** Coerce a stored cluster-member entry (v11+ or legacy v10) into the
 *  v11+ ClusterMember shape. Returns null for entries we can't make
 *  sense of. */
function _normalize(m: ClusterMember | LegacyMember): ClusterMember | null {
  if (!m || typeof m !== "object") return null;
  if ("kind" in m && m.kind) {
    if (m.kind === "track") {
      return {
        kind: "track",
        album_id: m.album_id,
        dir_name: m.dir_name,
        artist: m.artist,
        filenames: Array.isArray(m.filenames) ? m.filenames.filter(Boolean) : [],
        sort: m.sort ?? "",
      };
    }
    return {
      kind: "album",
      album_id: m.album_id,
      dir_name: m.dir_name,
      album_artist: m.album_artist,
      sort: m.sort ?? "",
    };
  }
  // Legacy v10 entry: album-kind only, "id" instead of "album_id",
  // "album_artist_sort" instead of "sort".
  const legacy = m as LegacyMember;
  const albumId = legacy.album_id ?? legacy.id;
  if (!albumId) return null;
  return {
    kind: "album",
    album_id: albumId,
    dir_name: legacy.dir_name,
    album_artist: legacy.album_artist,
    sort: (legacy.album_artist_sort ?? "") || "",
  };
}

function _creditOf(m: ClusterMember | undefined): string {
  if (!m) return "";
  if (m.kind === "track") return m.artist || "";
  return m.album_artist || "";
}

/** Conservative sort-suggestion heuristic.
 *
 *  We ONLY rotate articles ("The Beatles" → "Beatles, The"), which is
 *  deterministic and always correct. We deliberately DON'T do
 *  "First Last" → "Last, First" — that heuristic looks tempting but
 *  false-positives on band names like "Mannheim Steamroller" (would
 *  wrongly suggest "Steamroller, Mannheim") and "Steely Dan" /
 *  "Pink Floyd" / etc. Distinguishing personal names from band names
 *  reliably needs MusicBrainz data, which we don't have on the client.
 *
 *  When in doubt, return the credit unchanged and let the user type
 *  what they want. Better to under-suggest than mislead the user
 *  into accepting a wrong rotation. */
function _autoSuggestSort(credit: string): string {
  const trimmed = credit.trim();
  if (!trimmed) return "";
  const articleMatch = trimmed.match(/^(The|A|An)\s+(.+)$/i);
  if (articleMatch) {
    return `${articleMatch[2]}, ${articleMatch[1]}`;
  }
  return trimmed;
}
