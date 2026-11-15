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
 *
 * Composed inside `FindingsSection` (these corrections are a kind of
 * finding). Render with `bare` to drop the outer Section chrome when
 * embedding inside another section's body.
 */
import { useMemo, useState } from "react";
import { CheckCircle2, Loader2 } from "lucide-react";

import { useAlbumResolution } from "@/hooks/useAlbumResolution";
import { useTracks } from "@/hooks/useTracks";
import { useAiStatus } from "@/hooks/useAiStatus";
import type { Album, AlbumResolution, FieldFact } from "@/lib/types";
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
  /** Drop the outer <Section> wrapper. Used when this is being
   *  composed inside another section's body (e.g. FindingsSection). */
  bare?: boolean;
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

/**
 * Hook returning the diff payload an `MbPreviewSection` would render.
 * Exposed so parent surfaces (e.g. FindingsSection) can decide
 * whether to render at all — without duplicating the resolver+tracks
 * plumbing here.
 */
export function useMbPreviewDiffs(album: Album): {
  albumDiffs: AlbumFieldDiff[];
  trackDiffs: TrackFieldDiff[];
  resolution: AlbumResolution | undefined;
  isLoading: boolean;
} {
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

  return { albumDiffs, trackDiffs, resolution, isLoading };
}

export default function MbPreviewSection({
  album, hasActiveJob, applying, onApply, bare = false,
}: Props) {
  const { albumDiffs, trackDiffs, resolution, isLoading } = useMbPreviewDiffs(album);
  // "no AI quota" only means something when a model can run.
  const ai = useAiStatus();
  const quotaNote = ai.ready ? "deterministic · no AI quota" : "deterministic";

  // User-deselected row keys. Default-on: a row is included unless its
  // key is in this set. Lets the user skip individual MB diffs they
  // don't trust (e.g. a release with an MB typo) without dismissing
  // the whole batch.
  const [excluded, setExcluded] = useState<Set<string>>(() => new Set());

  // Suppress when:
  // - we have nothing to show
  // - an AI job is in flight (don't compete with that surface)
  // - the resolution still hasn't loaded (the badge in AlbumHero
  //   covers the loading state already)
  if (hasActiveJob) return null;
  if (isLoading) return null;
  if (!resolution || resolution.anchor === null) return null;
  if (albumDiffs.length === 0 && trackDiffs.length === 0) return null;

  const albumKey = (d: AlbumFieldDiff) => `album:${d.field}`;
  const trackKey = (d: TrackFieldDiff) => `track:${d.filename}:${d.field}`;
  const isChecked = (key: string) => !excluded.has(key);
  const toggle = (key: string) =>
    setExcluded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });

  const selectedAlbumDiffs = albumDiffs.filter((d) => isChecked(albumKey(d)));
  const selectedTrackDiffs = trackDiffs.filter((d) => isChecked(trackKey(d)));
  const totalChanges = albumDiffs.length + trackDiffs.length;
  const selectedCount = selectedAlbumDiffs.length + selectedTrackDiffs.length;

  const onClickApply = async () => {
    const albumSugs: Record<string, string> = {};
    for (const d of selectedAlbumDiffs) albumSugs[d.field] = d.canonical;
    const trackSugs: Record<string, Record<string, string>> = {};
    for (const d of selectedTrackDiffs) {
      if (!trackSugs[d.filename]) trackSugs[d.filename] = {};
      trackSugs[d.filename][d.field] = d.canonical;
    }
    await onApply(albumSugs, trackSugs);
  };

  const body = (
    <div className={`px-4 py-3 space-y-3 text-sm ${bare ? "border-b border-line bg-bg-2" : ""}`}>
      {bare && (
        <div className="flex items-center justify-between gap-2 -mx-1">
          <div className="mono text-[10.5px] uppercase tracking-[0.06em] text-emerald-400">
            MusicBrainz corrections ({totalChanges}) · {quotaNote}
          </div>
        </div>
      )}
        {albumDiffs.length > 0 && (
          <div className="space-y-1.5">
            {albumDiffs.map((d) => {
              const key = albumKey(d);
              return (
                <DiffRow
                  key={key}
                  label={ALBUM_FIELD_LABELS[d.field]}
                  current={d.current}
                  canonical={d.canonical}
                  checked={isChecked(key)}
                  onToggle={() => toggle(key)}
                />
              );
            })}
          </div>
        )}
        {trackDiffs.length > 0 && (
          <div className="space-y-1 pt-2 border-t border-line">
            <div className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4 mb-1.5">
              Per-track ({trackDiffs.length})
            </div>
            {trackDiffs.map((d, i) => {
              const key = trackKey(d);
              return (
                <DiffRow
                  key={`${key}-${i}`}
                  label={`${d.filename} · ${TRACK_FIELD_LABELS[d.field]}`}
                  current={d.current}
                  canonical={d.canonical}
                  compact
                  checked={isChecked(key)}
                  onToggle={() => toggle(key)}
                />
              );
            })}
          </div>
        )}
        <div className="flex items-center gap-2 pt-2">
          <button
            onClick={onClickApply}
            disabled={applying || selectedCount === 0}
            className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-60 inline-flex items-center gap-1.5"
          >
            {applying ? (
              <><Loader2 className="w-3.5 h-3.5 spin" /> Applying…</>
            ) : (
              <><CheckCircle2 className="w-3.5 h-3.5" /> Apply {selectedCount} change{selectedCount === 1 ? "" : "s"}</>
            )}
          </button>
          <span className="text-xs text-fg-4">
            Source: {resolution.anchor.source}
            {resolution.anchor_strategy !== "none" && ` (${resolution.anchor_strategy})`}
          </span>
        </div>
    </div>
  );

  if (bare) return body;

  return (
    <Section
      id="mb-preview"
      title={`MusicBrainz corrections (${selectedCount}/${totalChanges})`}
      defaultOpen
      meta={
        <span className="mono text-[10.5px] text-emerald-400">
          {quotaNote}
        </span>
      }
    >
      {body}
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
  label, current, canonical, compact = false, checked, onToggle,
}: {
  label: string;
  current: string;
  canonical: string;
  compact?: boolean;
  checked: boolean;
  onToggle: () => void;
}) {
  const [curParts, canParts] = diffSegments(current, canonical);
  // When the row is unchecked, dim the diff bodies so it's visually
  // clear they won't be applied — but keep the checkbox + label fully
  // legible so the user can re-enable easily.
  const dim = checked ? "" : "opacity-40";
  return (
    <label className={`grid items-baseline gap-2 cursor-pointer ${compact ? "text-xs" : ""}`} style={{ gridTemplateColumns: "auto auto 1fr auto 1fr" }}>
      <input
        type="checkbox"
        checked={checked}
        onChange={onToggle}
        className="w-3.5 h-3.5 accent-accent translate-y-[1px]"
        aria-label={`Apply ${label} correction`}
      />
      <span className={`mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4 truncate ${dim}`}>
        {label}
      </span>
      <span className={`text-fg-3 truncate line-through opacity-70 ${dim}`} title={diffTitle(current, canonical, "current")}>
        {current
          ? curParts.map((p, i) =>
              p.changed
                ? <span key={i} className="bg-amber-500/25 text-amber-200 no-underline rounded-sm px-0.5">{visualize(p.text)}</span>
                : <span key={i}>{p.text}</span>
            )
          : <em className="not-italic text-fg-4">(empty)</em>}
      </span>
      <span className={`text-fg-4 ${dim}`}>→</span>
      <span className={`text-fg truncate font-medium ${dim}`} title={diffTitle(current, canonical, "canonical")}>
        {canParts.map((p, i) =>
          p.changed
            ? <span key={i} className="bg-emerald-500/25 text-emerald-200 rounded-sm px-0.5">{visualize(p.text)}</span>
            : <span key={i}>{p.text}</span>
        )}
      </span>
    </label>
  );
}

interface Seg { text: string; changed: boolean }

/** Split two strings into [equal-prefix, differing-middle, equal-suffix]
 *  segments. Cheap, no LCS — perfect for the short title/artist strings
 *  this UI handles, and lets visually-subtle diffs (smart quotes, em
 *  dashes, NBSP) jump out via the highlight in DiffRow. */
function diffSegments(a: string, b: string): [Seg[], Seg[]] {
  const aChars = [...a];
  const bChars = [...b];
  let pre = 0;
  while (pre < aChars.length && pre < bChars.length && aChars[pre] === bChars[pre]) pre++;
  let suf = 0;
  while (
    suf < aChars.length - pre &&
    suf < bChars.length - pre &&
    aChars[aChars.length - 1 - suf] === bChars[bChars.length - 1 - suf]
  ) suf++;
  const aMid = aChars.slice(pre, aChars.length - suf).join("");
  const bMid = bChars.slice(pre, bChars.length - suf).join("");
  const prefix = aChars.slice(0, pre).join("");
  const suffix = aChars.slice(aChars.length - suf).join("");
  const aSegs: Seg[] = [];
  const bSegs: Seg[] = [];
  if (prefix) { aSegs.push({ text: prefix, changed: false }); bSegs.push({ text: prefix, changed: false }); }
  if (aMid)   aSegs.push({ text: aMid, changed: true });
  if (bMid)   bSegs.push({ text: bMid, changed: true });
  if (suffix) { aSegs.push({ text: suffix, changed: false }); bSegs.push({ text: suffix, changed: false }); }
  return [aSegs, bSegs];
}

/** Make invisible characters (NBSP, zero-width, etc.) visible in the
 *  diff highlight. Keeps printable characters as-is so smart quotes
 *  still render naturally — the highlight is what flags them. */
function visualize(s: string): string {
  return s
    .replace(/ /g, "␣")  // NBSP
    .replace(/​/g, "·")   // ZWSP
    .replace(/‎|‏|﻿/g, "·"); // bidi marks, BOM
}

/** Tooltip surfaces the codepoints of the differing region so users
 *  can disambiguate "Theme" vs "Them" from "smart-quote vs ASCII". */
function diffTitle(current: string, canonical: string, side: "current" | "canonical"): string {
  const [aSegs, bSegs] = diffSegments(current, canonical);
  const segs = side === "current" ? aSegs : bSegs;
  const target = side === "current" ? current : canonical;
  const changed = segs.find((s) => s.changed);
  if (!changed) return target;
  const codes = [...changed.text].map((c) => `U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}`).join(" ");
  return `${target}\n\nDiffering: "${changed.text}"  [${codes}]`;
}
