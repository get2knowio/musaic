import { useMemo } from "react";
import { useTracks } from "@/hooks/useTracks";
import type {
  Album,
  CoverSuggestionResponse,
  MetadataSuggestionResponse,
  Track,
  TrackSuggestionEntry,
} from "@/lib/types";

interface AISuggestionsProps {
  album: Album;
  metadata: MetadataSuggestionResponse | null;
  cover: CoverSuggestionResponse | null;
  onAccept: () => void;
  onDiscard: () => void;
  /** Wipe the cached MB anchor + discard these suggestions. Escape
   *  hatch for the "wrong release-group locked in" case where the
   *  resolver synthesizes the same wrong cover/metadata every time
   *  because the album's `mb_release_group_mbid` is cached against the
   *  wrong release. */
  onClearAnchor: () => void;
  /** Whether the cached anchor is in flight being cleared — disables
   *  the button + shows progress. */
  clearingAnchor: boolean;
  applying: boolean;
}

const FIELD_LABELS: Record<string, string> = {
  title: "title",
  album_artist: "album artist",
  artist: "artist",
  year: "year",
  genre: "genre",
  tracknumber: "#",
};

const TRACK_FIELD_ORDER: (keyof TrackSuggestionEntry["suggestions"])[] = [
  "tracknumber",
  "title",
  "artist",
];

/**
 * Renders Claude's metadata suggestions as before/after diff rows, plus the
 * per-track diff table and suggested cover (if any). Lives in the sheet body,
 * replaces the standard sections during the suggest flow.
 */
export default function AISuggestions({
  album,
  metadata,
  cover,
  onAccept,
  onDiscard,
  onClearAnchor,
  clearingAnchor,
  applying,
}: AISuggestionsProps) {
  // We need the live track listing to compute "before" values for per-track diffs.
  const tracksQuery = useTracks(album.id);
  const tracksByName = useMemo(
    () => new Map((tracksQuery.data?.tracks ?? []).map((t) => [t.filename, t])),
    [tracksQuery.data],
  );

  const sug = metadata?.suggestions ?? {};
  const albumKeys = Object.keys(sug);
  const trackEntries = metadata?.tracks ?? [];
  const hasAlbumChanges = albumKeys.length > 0;
  const hasTrackChanges = trackEntries.length > 0;
  const hasCover = !!cover?.cover_url;
  // Whether each subcall was actually performed (the prop is non-null when
  // we ran the corresponding sub-job, regardless of what it returned).
  const metadataWasRequested = metadata !== null;
  const coverWasRequested = cover !== null;
  // Cover was searched but came back empty — we want to surface this in the
  // UI rather than silently hide the cover section.
  const coverSearchEmpty = coverWasRequested && !hasCover;

  // Pre-compute which per-track entries actually represent changes (vs the
  // model returning the same value back to us).
  const realTrackChanges = trackEntries.filter((entry) => {
    const before = tracksByName.get(entry.filename);
    if (!before) return true;  // can't verify, show it
    return Object.entries(entry.suggestions).some(
      ([k, v]) => String(before[k as keyof Track] ?? "") !== String(v ?? ""),
    );
  });
  const renameCount = realTrackChanges.filter((entry) => {
    if (!entry.suggestions.tracknumber) return false;
    return /^\d+(?:[-.]\d+)?[\s\-_.]/.test(entry.filename);
  }).length;

  // MBID side-effect status. The AI can identify the album on MB
  // independently of whether it proposes any tag corrections — and
  // when it does, the worker has already persisted it to the cache
  // by the time the user sees this UI. We surface it explicitly so
  // the user can SEE that side-effect happened (or that it didn't,
  // if the unidentified-album finding is staying open).
  const mbid = metadata?.mb_release_group_mbid ?? null;
  const mbidStatus: "identified" | "not-identified" | "n/a" =
    metadataWasRequested
      ? (mbid ? "identified" : "not-identified")
      : "n/a";

  if (!hasAlbumChanges && !hasTrackChanges && !hasCover) {
    // Build a context-aware message based on what we actually searched.
    // The original "metadata looks accurate" line was misleading for cover-only
    // jobs and for combo jobs where cover ALSO failed silently.
    let primary: string;
    if (metadataWasRequested && coverWasRequested) {
      primary = mbidStatus === "identified"
        ? "No tag corrections needed, and no cover available — but the album was identified on MusicBrainz."
        : "No metadata fixes found, and we couldn't identify a cover for this album.";
    } else if (metadataWasRequested) {
      primary = mbidStatus === "identified"
        ? "No tag corrections needed — current metadata looks accurate. The album was identified on MusicBrainz; the unidentified-album finding will clear momentarily."
        : "No high-confidence corrections, and Claude couldn't identify this on MusicBrainz either. Try editing the directory name or adding a hint.";
    } else {
      primary = "Couldn't identify a cover for this album.";
    }
    return (
      <div className="px-4 py-4 space-y-3">
        <div className="border border-line rounded-md bg-panel-2 p-3 text-sm">
          <div className="mono text-xs uppercase tracking-[0.06em] text-fg-4 mb-2">
            Claude's review
          </div>
          <p className="text-fg-3 italic text-sm">{primary}</p>
          {mbidStatus !== "n/a" && (
            <MbidStatusLine status={mbidStatus} mbid={mbid} />
          )}
          {metadataWasRequested && metadata?.notes && (
            <p className="mt-2 text-xs text-fg-2 leading-relaxed">
              <strong className="text-fg">Metadata notes:</strong> {metadata.notes}
            </p>
          )}
          {coverWasRequested && cover?.notes && (
            <p className="mt-2 text-xs text-fg-2 leading-relaxed">
              <strong className="text-fg">Cover-art notes:</strong> {cover.notes}
            </p>
          )}
        </div>
        <div className="flex gap-2">
          <button
            onClick={onDiscard}
            className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition"
          >
            Discard
          </button>
          <ClearAnchorButton onClick={onClearAnchor} busy={clearingAnchor} />
        </div>
      </div>
    );
  }

  return (
    <div className="px-4 py-4 space-y-3">
      {/* MBID identification status. Independent of the tag/cover diff —
          the MBID was persisted server-side as soon as the job completed
          (validated UUID + verified live against MB). Surfacing it here
          so the user can see the unidentified-album side-effect happened. */}
      {mbidStatus !== "n/a" && (
        <div className="border border-line rounded-md bg-panel-2 p-3 text-sm">
          <MbidStatusLine status={mbidStatus} mbid={mbid} />
        </div>
      )}

      {/* Album-level diff */}
      {hasAlbumChanges && metadata && (
        <div className="border border-accent/30 rounded-md bg-accent-soft/40 p-3 text-sm">
          <div className="flex items-center justify-between mb-2.5">
            <span className="mono text-xs uppercase tracking-[0.06em] text-fg-3">
              Album-level changes
            </span>
            <ConfidenceBadge confidence={metadata.confidence} />
          </div>
          <div className="space-y-2">
            {albumKeys.map((k) => {
              const oldVal = (album as unknown as Record<string, string | null>)[k];
              const newVal = sug[k as keyof typeof sug] ?? "";
              if (String(oldVal ?? "") === String(newVal)) {
                return (
                  <DiffRow key={k} label={FIELD_LABELS[k] ?? k}>
                    <span className="text-fg-4 italic">{newVal} (no change)</span>
                  </DiffRow>
                );
              }
              return (
                <DiffRow key={k} label={FIELD_LABELS[k] ?? k}>
                  <span className="line-through text-err text-xs block">
                    {String(oldVal ?? "—")}
                  </span>
                  <span className="text-ok font-medium block">{newVal}</span>
                </DiffRow>
              );
            })}
          </div>
        </div>
      )}

      {/* Per-track diff */}
      {realTrackChanges.length > 0 && (
        <div className="border border-accent/30 rounded-md bg-accent-soft/40 overflow-hidden text-sm">
          <div className="flex items-center justify-between px-3 py-2 border-b border-line">
            <span className="mono text-xs uppercase tracking-[0.06em] text-fg-3">
              Per-track changes ({realTrackChanges.length} track{realTrackChanges.length === 1 ? "" : "s"})
              {renameCount > 0 && (
                <span className="ml-2 mono text-fg-4 normal-case tracking-normal">
                  · {renameCount} file{renameCount === 1 ? "" : "s"} will be renamed
                </span>
              )}
            </span>
          </div>
          <div className="divide-y divide-line">
            {realTrackChanges.map((entry) => {
              const before = tracksByName.get(entry.filename);
              return (
                <TrackDiffRow
                  key={entry.filename}
                  filename={entry.filename}
                  before={before}
                  after={entry.suggestions}
                />
              );
            })}
          </div>
        </div>
      )}

      {/* Notes (album-level, applies to whole suggestion) */}
      {metadata?.notes && (
        <div className="border border-line rounded-md bg-panel-2 p-3 text-xs text-fg-2 leading-relaxed">
          <strong className="text-fg">Notes:</strong> {metadata.notes}
        </div>
      )}

      {/* Cover lookup ran but came back empty — surface this so the user
          knows Claude tried (rather than silently omitting the cover slot). */}
      {coverSearchEmpty && (
        <div className="border border-line rounded-md bg-panel-2 p-3 text-sm">
          <div className="mono text-xs uppercase tracking-[0.06em] text-fg-4 mb-1">
            Cover art
          </div>
          <p className="text-fg-3 text-xs italic">
            Couldn't identify a confident cover for this album.
          </p>
          {cover?.notes && (
            <p className="mt-1.5 text-xs text-fg-2 leading-relaxed">{cover.notes}</p>
          )}
        </div>
      )}

      {/* Cover suggestion */}
      {hasCover && cover && (
        <div className="border border-line rounded-md bg-panel-2 p-3 text-sm flex gap-3">
          <img
            src={cover.cover_url ?? ""}
            alt="proposed cover"
            className="w-20 h-20 rounded object-cover bg-bg-2 shrink-0"
            onError={(e) => {
              e.currentTarget.style.display = "none";
            }}
          />
          <div className="flex-1 min-w-0">
            <div className="flex items-center justify-between mb-1">
              <span className="mono text-xs uppercase tracking-[0.06em] text-fg-3">
                Proposed cover
              </span>
              <ConfidenceBadge confidence={cover.confidence} />
            </div>
            <div className="text-xs text-fg-3 mt-1">{cover.source}</div>
            <a
              href={cover.cover_url ?? "#"}
              target="_blank"
              rel="noopener"
              className="text-xs text-accent hover:underline break-all block mt-1"
            >
              {cover.cover_url}
            </a>
            {cover.notes && (
              <div className="mt-2 text-xs text-fg-2 leading-relaxed">{cover.notes}</div>
            )}
          </div>
        </div>
      )}

      <div className="flex items-center gap-2 pt-1">
        <button
          onClick={onDiscard}
          disabled={applying}
          className="h-8 px-3 border border-line rounded text-sm hover:bg-bg-3 transition disabled:opacity-60"
        >
          Discard
        </button>
        <ClearAnchorButton onClick={onClearAnchor} busy={clearingAnchor} disabled={applying} />
        <button
          onClick={onAccept}
          disabled={applying}
          title={applyButtonTooltip(albumKeys.length, realTrackChanges.length, renameCount, hasCover)}
          className="h-8 px-3 rounded bg-accent text-accent-fg font-medium text-sm hover:opacity-90 transition disabled:opacity-60 ml-auto truncate max-w-[260px]"
        >
          {applying ? "Applying…" : applyButtonLabel(albumKeys.length, realTrackChanges.length, renameCount, hasCover)}
        </button>
      </div>
    </div>
  );
}

/** "Wrong match?" escape hatch — clears the cached MB anchor so the
 *  next AI run isn't short-circuited by a synthesizer that keeps
 *  returning the same wrong cover/title from a stale release-group. */
function ClearAnchorButton({
  onClick, busy, disabled = false,
}: {
  onClick: () => void;
  busy: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={busy || disabled}
      title="Wipe the cached MusicBrainz match so a new AI run can search again. Use when the wrong release-group has been locked in (common for compilations). Tip: re-run with a hint to bias the search."
      className="h-8 px-3 border border-amber-500/40 text-amber-300 rounded text-sm hover:bg-amber-500/10 transition disabled:opacity-60"
    >
      {busy ? "Clearing…" : "Wrong match"}
    </button>
  );
}

/** Compact label — fits inside the diff view's 260px-cap button without
 *  wrapping. The full prose version is in `applyButtonTooltip` and surfaces
 *  on hover. */
function applyButtonLabel(albumCount: number, trackCount: number, renameCount: number, hasCover: boolean): string {
  const parts: string[] = [];
  if (albumCount > 0) parts.push(`${albumCount} album`);
  if (trackCount > 0) parts.push(`${trackCount} track${trackCount === 1 ? "" : "s"}`);
  if (renameCount > 0) parts.push(`${renameCount} rename${renameCount === 1 ? "" : "s"}`);
  if (hasCover) parts.push("cover");
  if (parts.length === 0) return "Apply";
  return `Apply (${parts.join(", ")})`;
}

/** Long-form description used in `title` tooltip — same content as the old
 *  inline label, kept around so the user can still see the detailed
 *  breakdown by hovering. */
function applyButtonTooltip(albumCount: number, trackCount: number, renameCount: number, hasCover: boolean): string {
  const parts: string[] = [];
  if (albumCount > 0) parts.push(`${albumCount} album field${albumCount === 1 ? "" : "s"}`);
  if (trackCount > 0) {
    const renameSuffix = renameCount > 0 ? `, ${renameCount} rename${renameCount === 1 ? "" : "s"}` : "";
    parts.push(`${trackCount} track${trackCount === 1 ? "" : "s"}${renameSuffix}`);
  }
  if (hasCover) parts.push("cover");
  if (parts.length === 0) return "Apply changes";
  return `Apply ${parts.join(" + ")}`;
}

function TrackDiffRow({
  filename,
  before,
  after,
}: {
  filename: string;
  before: Track | undefined;
  after: TrackSuggestionEntry["suggestions"];
}) {
  const renameTarget = predictRenameTarget(filename, after.tracknumber);

  return (
    <div className="px-3 py-2 text-sm">
      <div className="mono text-xs text-fg-4 mb-1.5 truncate" title={filename}>
        {renameTarget && renameTarget !== filename ? (
          <>
            <span className="line-through text-err">{filename}</span>{" "}
            <span className="text-fg-3">→</span>{" "}
            <span className="text-ok">{renameTarget}</span>
          </>
        ) : (
          filename
        )}
      </div>
      <div className="grid grid-cols-[64px_1fr] gap-x-2 gap-y-1">
        {TRACK_FIELD_ORDER.filter((k) => after[k] !== undefined).map((k) => {
          const oldVal = before ? (before as unknown as Record<string, string | null>)[k] : null;
          const newVal = after[k] ?? "";
          const unchanged = String(oldVal ?? "") === String(newVal);
          return (
            <DiffRow key={k} label={FIELD_LABELS[k] ?? k}>
              {unchanged ? (
                <span className="text-fg-4 italic">{newVal} (no change)</span>
              ) : (
                <>
                  <span className="line-through text-err text-xs">{String(oldVal ?? "—")}</span>
                  <span className="mx-1.5 text-fg-4">→</span>
                  <span className="text-ok font-medium">{newVal}</span>
                </>
              )}
            </DiffRow>
          );
        })}
      </div>
    </div>
  );
}

/** Mirrors `_proposed_filename` in tags.py — same regex, same padding. */
function predictRenameTarget(filename: string, newTrackNumber: string | undefined): string | null {
  if (!newTrackNumber) return null;
  const m = filename.match(/^(\d+(?:[-.]\d+)?)([\s\-_.]+)/);
  if (!m) return null;
  const n = parseInt(newTrackNumber, 10);
  if (isNaN(n)) return null;
  const padded = n.toString().padStart(2, "0");
  return padded + filename.slice(m[1].length);
}

function DiffRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <span className="mono text-xs text-fg-4 self-baseline">{label}</span>
      <div className="text-sm">{children}</div>
    </>
  );
}

function ConfidenceBadge({ confidence }: { confidence: string }) {
  const color =
    confidence === "high"
      ? "bg-ok-soft text-ok border-ok/30"
      : confidence === "medium"
      ? "bg-warn-soft text-warn border-warn/30"
      : "bg-bg-3 text-fg-3 border-line";
  return (
    <span
      className={`mono text-[10px] uppercase tracking-wider px-1.5 py-0.5 rounded-full border ${color}`}
    >
      {confidence}
    </span>
  );
}

/** Status line telling the user whether AI identified the album on
 *  MusicBrainz. Shown both in the empty-state panel and above the diff
 *  view when changes are present. The MBID has already been persisted
 *  by the time this renders — this is purely a "we did this" notice,
 *  not an action the user accepts. Linked to MB so the user can verify
 *  by hand. */
function MbidStatusLine({
  status,
  mbid,
}: {
  status: "identified" | "not-identified" | "n/a";
  mbid: string | null;
}) {
  if (status === "n/a") return null;
  if (status === "identified" && mbid) {
    return (
      <div className="mt-2 flex items-start gap-2 text-xs text-fg-2 leading-relaxed">
        <span className="mono uppercase tracking-[0.06em] text-ok shrink-0">
          MusicBrainz:
        </span>
        <span className="min-w-0">
          identified as{" "}
          <a
            href={`https://musicbrainz.org/release-group/${mbid}`}
            target="_blank"
            rel="noopener noreferrer"
            className="mono text-accent hover:underline break-all"
          >
            {mbid}
          </a>
          {" "}— verified and saved. The unidentified-album finding will clear momentarily.
        </span>
      </div>
    );
  }
  // not-identified
  return (
    <div className="mt-2 flex items-start gap-2 text-xs text-fg-3 leading-relaxed">
      <span className="mono uppercase tracking-[0.06em] text-fg-4 shrink-0">
        MusicBrainz:
      </span>
      <span>
        not identified. The unidentified-album finding stays open — try editing the
        directory name to canonical "Artist - Album" form, or click Fix again
        with a hint.
      </span>
    </div>
  );
}

