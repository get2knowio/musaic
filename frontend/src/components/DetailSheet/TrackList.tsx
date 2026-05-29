import { CheckCircle2, Circle, Fingerprint, Loader2, XCircle } from "lucide-react";
import { displayArtist } from "@/lib/albumFields";
import type { Album, AlbumResolution, Track } from "@/lib/types";
import type { ApplyFileState } from "@/hooks/useApplyStream";

interface TrackListProps {
  album: Album;
  tracks: Track[];
  /** When applying, show per-file status icons + highlight active row. */
  applyState?: Record<number, ApplyFileState>;
  /** Optional resolver output — drives per-track fingerprint indicators. */
  resolution?: AlbumResolution;
}

/**
 * Compact track grid. Columns adapt to:
 *   - whether this is a compilation (adds an artist column)
 *   - whether BPM/RG values exist on any track (adds those columns)
 *   - whether an apply is in progress (adds a status icon column)
 */
export default function TrackList({ album, tracks, applyState, resolution }: TrackListProps) {
  // Show the artist column when either:
  //   - the directory parser flagged this as a compilation, OR
  //   - any track's artist differs from the album_artist (covers
  //     compilations whose folder doesn't follow the `[FLAC]`
  //     suffix convention, plus albums with guest credits).
  // Falls back to false when all tracks share the album artist (or are
  // blank) — column would be redundant in that case.
  const albumArtistNorm = displayArtist(album).toLowerCase();
  const hasVaryingArtists = tracks.some((t) => {
    const trackArtist = (t.artist || "").trim().toLowerCase();
    return trackArtist && trackArtist !== albumArtistNorm;
  });
  const showArtist = album.is_compilation || hasVaryingArtists;
  const showBpm = tracks.some((t) => t.bpm !== null);
  const showRg = tracks.some((t) => t.replaygain_track_gain);
  const showStatus = applyState !== undefined && Object.keys(applyState).length > 0;
  // filename → fingerprint state for the per-track chip. Three
  // visual states:
  //   - matched   : Chromaprint computed AND AcoustID returned a
  //                 confident recording_mbid. Green icon, links to
  //                 the MB recording page.
  //   - unmatched : Chromaprint computed BUT AcoustID had no match
  //                 (common for obscure / album-original tracks
  //                 that few people have submitted). Yellow icon,
  //                 not a link — the icon's only purpose is to
  //                 communicate "we tried; nothing confident came
  //                 back."
  //   - missing   : no Chromaprint row at all. No icon. The eager
  //                 fingerprint pool usually covers every track
  //                 post-scan, so this is rare (fpcalc not
  //                 installed, or scan not yet complete).
  //
  // The signal for "Chromaprint computed" is `t.fingerprinted` (a
  // boolean from the backend that's True iff a track_fingerprints
  // row exists). We can NOT use `fingerprint_score` for this: it
  // gets NULLed both for "lookup never ran" and for "lookup ran
  // and returned no confident match" — same value for both states.
  type FpState = { kind: "matched"; mbid: string } | { kind: "unmatched" };
  const fingerprintByFilename = new Map<string, FpState>(
    (resolution?.tracks ?? [])
      .map((t): [string, FpState | null] => {
        if (t.recording_mbid) {
          return [t.filename, { kind: "matched", mbid: t.recording_mbid }];
        }
        if (t.fingerprinted) {
          return [t.filename, { kind: "unmatched" }];
        }
        return [t.filename, null];
      })
      .filter((entry): entry is [string, FpState] => entry[1] !== null)
      .map(([fn, state]) => [fn, state]),
  );

  // Build a CSS grid template for the row.
  const cols = [
    showStatus && "20px",
    "32px",                                // track number
    "minmax(0, 1.4fr)",                    // title
    showArtist && "minmax(0, 1fr)",        // artist
    showBpm && "56px",                     // BPM
    showRg && "72px",                      // RG track gain
    "60px",                                // duration
  ].filter(Boolean).join(" ");

  return (
    <div className="px-4 py-3">
      <div className="border border-line rounded-md overflow-hidden bg-panel-2">
        <div
          className="grid gap-2 px-3 py-2 border-b border-line text-fg-4 mono text-[10.5px] uppercase tracking-[0.06em]"
          style={{ gridTemplateColumns: cols }}
        >
          {showStatus && <span />}
          <span className="text-right">#</span>
          <span>Title</span>
          {showArtist && <span>Artist</span>}
          {showBpm && <span className="text-right">BPM</span>}
          {showRg && <span className="text-right">RG track</span>}
          <span className="text-right">Length</span>
        </div>
        {tracks.map((t, i) => {
          const trackIndex = i;
          const fileState = applyState?.[trackIndex];
          const status = fileState?.status;
          const rowBg =
            status === "active" ? "bg-accent-soft"
            : status === "error" ? "bg-err-soft"
            : status === "done" ? "" // success styling is on the icon
            : "";

          return (
            <div
              key={t.filename + i}
              className={`grid gap-2 px-3 py-1.5 border-b border-line last:border-0 items-center text-sm ${rowBg}`}
              style={{ gridTemplateColumns: cols }}
            >
              {showStatus && <StatusIcon status={status ?? "pending"} />}
              <span
                className="mono tabular-nums text-fg-4 text-right"
                title={t.filename}
              >
                {t.tracknumber || ""}
              </span>
              {/* Hover shows the actual filename — directory is visible
                  in the album header but per-file names weren't surfaced
                  anywhere before. Helps when investigating duplicates,
                  weird naming, or just figuring out which file is which. */}
              <span
                className="truncate inline-flex items-center gap-1.5 min-w-0"
                title={
                  t.title
                    ? `${t.title}\n${t.filename}`
                    : t.filename
                }
              >
                <span className="truncate">{t.title || t.filename}</span>
                {(() => {
                  const fp = fingerprintByFilename.get(t.filename);
                  if (!fp) return null;
                  if (fp.kind === "matched") {
                    return (
                      <a
                        href={`https://musicbrainz.org/recording/${fp.mbid}`}
                        target="_blank"
                        rel="noreferrer"
                        title="Identified via AcoustID fingerprint — click to view on MusicBrainz"
                        className="text-emerald-400 hover:text-emerald-300 shrink-0"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <Fingerprint className="w-3 h-3" />
                      </a>
                    );
                  }
                  // kind === "unmatched": rendered as a non-link span.
                  // Yellow signals "we tried but AcoustID didn't
                  // recognize this recording" — distinguishable at a
                  // glance from "fully identified" (green) and from
                  // "never fingerprinted" (no icon at all).
                  return (
                    <span
                      title="Fingerprinted, but AcoustID had no confident match for this recording. Common for obscure / album-original tracks."
                      className="text-amber-400 shrink-0"
                    >
                      <Fingerprint className="w-3 h-3" />
                    </span>
                  );
                })()}
              </span>
              {showArtist && (
                <span
                  className="truncate text-fg-3 text-xs"
                  title={t.artist ? `${t.artist}\n${t.filename}` : t.filename}
                >
                  {t.artist || ""}
                </span>
              )}
              {showBpm && (
                <span className={`mono tabular-nums text-right ${t.bpm !== null ? "text-fg-2" : "text-fg-4"}`}>
                  {t.bpm !== null ? t.bpm.toFixed(1) : "—"}
                </span>
              )}
              {showRg && (
                <span className={`mono tabular-nums text-right text-xs ${t.replaygain_track_gain ? "text-fg-2" : "text-fg-4"}`}>
                  {/* Use Unicode minus, per design handoff */}
                  {t.replaygain_track_gain ? t.replaygain_track_gain.replace(/^-/, "−") : "—"}
                </span>
              )}
              <span className="mono tabular-nums text-fg-3 text-right">
                {t.duration !== null ? formatTrackDuration(t.duration) : "—"}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function StatusIcon({ status }: { status: ApplyFileState["status"] }) {
  if (status === "done") return <CheckCircle2 className="w-3.5 h-3.5 text-ok" />;
  if (status === "active") return <Loader2 className="w-3.5 h-3.5 text-accent spin" />;
  if (status === "error") return <XCircle className="w-3.5 h-3.5 text-err" />;
  return <Circle className="w-2 h-2 text-fg-4" />;
}

function formatTrackDuration(seconds: number): string {
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s.toString().padStart(2, "0")}`;
}
