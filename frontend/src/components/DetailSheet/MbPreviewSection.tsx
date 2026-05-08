/**
 * Inline preview of corrections derivable from the resolver's
 * MusicBrainz data — no job queue, no AI quota, no waiting.
 *
 * Visible only when the album has a confident anchor AND there's at
 * least one field where current_tags disagree with MB's canonical
 * value. Clicking Apply writes via the existing apply stream (same
 * mutation pipeline `services/apply.py:apply_album_changes_iter`),
 * with per-file SSE feedback in the surrounding sheet.
 *
 * On a clean apply, the resolver cache is invalidated server-side,
 * useAlbumResolution refetches, the diffs go away, and this section
 * unmounts itself.
 */
import { useMemo } from "react";
import { CheckCircle2, Loader2 } from "lucide-react";

import { useAlbumResolution } from "@/hooks/useAlbumResolution";
import { useTracks } from "@/hooks/useTracks";
import type { Album, FieldFact } from "@/lib/types";
import Section from "./Section";

interface Props {
  album: Album;
  /** Whether an AI job is currently active for this album (queued /
   *  running / completed-not-acted). When set, suppress the preview
   *  to avoid presenting two competing "what would change" surfaces. */
  hasActiveJob: boolean;
  /** Apply orchestrator from useApplyStream — same one the AI flow uses. */
  applying: boolean;
  onApply: (
    albumSuggestions: Record<string, string>,
    trackSuggestions: Record<string, Record<string, string>>,
  ) => Promise<void>;
}

interface AlbumFieldDiff {
  field: keyof typeof ALBUM_FIELD_LABELS;
  current: string;
  canonical: string;
  source: string;
}

interface TrackFieldDiff {
  filename: string;
  field: keyof typeof TRACK_FIELD_LABELS;
  current: string;
  canonical: string;
}

const ALBUM_FIELD_LABELS = {
  title: "title",
  album_artist: "album artist",
  year: "year",
  album_artist_sort: "album-artist sort",
} as const;

const TRACK_FIELD_LABELS = {
  title: "title",
  artist: "artist",
  tracknumber: "#",
} as const;

export default function MbPreviewSection({
  album, hasActiveJob, applying, onApply,
}: Props) {
  const { data: resolution, isLoading } = useAlbumResolution(album.id);
  const { data: tracksData } = useTracks(album.id);

  const tracksByFilename = useMemo(
    () => new Map((tracksData?.tracks ?? []).map((t) => [t.filename, t])),
    [tracksData],
  );

  const { albumDiffs, trackDiffs } = useMemo(() => {
    const albumDiffs: AlbumFieldDiff[] = [];
    const trackDiffs: TrackFieldDiff[] = [];
    if (!resolution || resolution.anchor === null) {
      return { albumDiffs, trackDiffs };
    }
    pushDiff(albumDiffs, "title", album.title, resolution.canonical_title);
    pushDiff(albumDiffs, "album_artist", album.album_artist, resolution.canonical_album_artist);
    pushDiff(albumDiffs, "year", album.year, resolution.canonical_year);
    pushDiff(albumDiffs, "album_artist_sort", album.album_artist_sort, resolution.canonical_album_artist_sort);
    for (const tr of resolution.tracks) {
      const current = tracksByFilename.get(tr.filename);
      if (!current) continue;
      pushTrackDiff(trackDiffs, tr.filename, "title", current.title, tr.canonical_title);
      pushTrackDiff(trackDiffs, tr.filename, "artist", current.artist, tr.canonical_artist);
      pushTrackDiff(trackDiffs, tr.filename, "tracknumber", current.tracknumber, tr.canonical_tracknumber);
    }
    return { albumDiffs, trackDiffs };
  }, [resolution, album, tracksByFilename]);

  // Suppress when:
  // - we have nothing to show
  // - an AI job is in flight (don't compete with that surface)
  // - the resolution still hasn't loaded (the badge in AlbumHero
  //   covers the loading state already)
  if (hasActiveJob) return null;
  if (isLoading) return null;
  if (!resolution || resolution.anchor === null) return null;
  if (albumDiffs.length === 0 && trackDiffs.length === 0) return null;

  const onClickApply = async () => {
    const albumSugs: Record<string, string> = {};
    for (const d of albumDiffs) albumSugs[d.field] = d.canonical;
    const trackSugs: Record<string, Record<string, string>> = {};
    for (const d of trackDiffs) {
      if (!trackSugs[d.filename]) trackSugs[d.filename] = {};
      trackSugs[d.filename][d.field] = d.canonical;
    }
    await onApply(albumSugs, trackSugs);
  };

  const totalChanges = albumDiffs.length + trackDiffs.length;

  return (
    <Section
      id="mb-preview"
      title={`MusicBrainz corrections (${totalChanges})`}
      defaultOpen
      meta={
        <span className="mono text-[10.5px] text-emerald-400">
          deterministic · no AI quota
        </span>
      }
    >
      <div className="px-4 py-3 space-y-3 text-sm">
        {albumDiffs.length > 0 && (
          <div className="space-y-1.5">
            {albumDiffs.map((d) => (
              <DiffRow
                key={d.field}
                label={ALBUM_FIELD_LABELS[d.field]}
                current={d.current}
                canonical={d.canonical}
              />
            ))}
          </div>
        )}
        {trackDiffs.length > 0 && (
          <div className="space-y-1 pt-2 border-t border-line">
            <div className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4 mb-1.5">
              Per-track ({trackDiffs.length})
            </div>
            {trackDiffs.map((d, i) => (
              <DiffRow
                key={`${d.filename}-${d.field}-${i}`}
                label={`${d.filename} · ${TRACK_FIELD_LABELS[d.field]}`}
                current={d.current}
                canonical={d.canonical}
                compact
              />
            ))}
          </div>
        )}
        <div className="flex items-center gap-2 pt-2">
          <button
            onClick={onClickApply}
            disabled={applying}
            className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
          >
            {applying ? (
              <><Loader2 className="w-3.5 h-3.5 spin" /> Applying…</>
            ) : (
              <><CheckCircle2 className="w-3.5 h-3.5" /> Apply {totalChanges} change{totalChanges === 1 ? "" : "s"}</>
            )}
          </button>
          <span className="text-xs text-fg-4">
            Source: {resolution.anchor.source}
            {resolution.anchor_strategy !== "none" && ` (${resolution.anchor_strategy})`}
          </span>
        </div>
      </div>
    </Section>
  );
}


// ---- Helpers ----

function pushDiff(
  out: AlbumFieldDiff[],
  field: keyof typeof ALBUM_FIELD_LABELS,
  current: string | null | undefined,
  fact: FieldFact | null,
): void {
  if (!fact) return;
  const proposed = String(fact.value ?? "").trim();
  const cur = String(current ?? "").trim();
  if (!proposed || normalize(proposed) === normalize(cur)) return;
  out.push({ field, current: cur, canonical: proposed, source: fact.source });
}

function pushTrackDiff(
  out: TrackFieldDiff[],
  filename: string,
  field: keyof typeof TRACK_FIELD_LABELS,
  current: string | null | undefined,
  fact: FieldFact | null,
): void {
  if (!fact) return;
  const proposed = String(fact.value ?? "").trim();
  const cur = String(current ?? "").trim();
  if (!proposed || normalize(proposed) === normalize(cur)) return;
  out.push({ filename, field, current: cur, canonical: proposed });
}

/** Same case+whitespace normalization the backend uses to decide
 *  whether a diff is "real" — keeps frontend/backend in agreement. */
function normalize(s: string): string {
  return s.toLowerCase().split(/\s+/).filter(Boolean).join(" ");
}

function DiffRow({
  label, current, canonical, compact = false,
}: {
  label: string;
  current: string;
  canonical: string;
  compact?: boolean;
}) {
  return (
    <div className={`grid items-baseline gap-2 ${compact ? "text-xs" : ""}`} style={{ gridTemplateColumns: "auto 1fr auto 1fr" }}>
      <span className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4 truncate">
        {label}
      </span>
      <span className={`text-fg-3 truncate line-through opacity-70 ${compact ? "" : ""}`} title={current}>
        {current || <em className="not-italic text-fg-4">(empty)</em>}
      </span>
      <span className="text-fg-4">→</span>
      <span className="text-fg truncate font-medium" title={canonical}>
        {canonical}
      </span>
    </div>
  );
}
