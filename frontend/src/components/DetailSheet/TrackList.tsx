import { CheckCircle2, Circle, Loader2, XCircle } from "lucide-react";
import type { Album, Track } from "@/lib/types";
import type { ApplyFileState } from "@/hooks/useApplyStream";

interface TrackListProps {
  album: Album;
  tracks: Track[];
  /** When applying, show per-file status icons + highlight active row. */
  applyState?: Record<number, ApplyFileState>;
}

/**
 * Compact track grid. Columns adapt to:
 *   - whether this is a compilation (adds an artist column)
 *   - whether BPM/RG values exist on any track (adds those columns)
 *   - whether an apply is in progress (adds a status icon column)
 */
export default function TrackList({ album, tracks, applyState }: TrackListProps) {
  const showArtist = album.is_compilation;
  const showBpm = tracks.some((t) => t.bpm !== null);
  const showRg = tracks.some((t) => t.replaygain_track_gain);
  const showStatus = applyState !== undefined && Object.keys(applyState).length > 0;

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
                className="truncate"
                title={
                  t.title
                    ? `${t.title}\n${t.filename}`
                    : t.filename
                }
              >
                {t.title || t.filename}
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
