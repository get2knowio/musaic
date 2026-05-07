import { Activity, BarChart3 } from "lucide-react";
import type { Album, Track } from "@/lib/types";

interface AudioAnalysisProps {
  album: Album;
  tracks: Track[];
}

/**
 * Read-only display of audio-analysis tags (BPM + ReplayGain) per the design
 * handoff. Calculate-buttons + the actual SSE analysis endpoints land in
 * the next milestone — for now, we just visualize what's already there.
 */
export default function AudioAnalysis({ album, tracks }: AudioAnalysisProps) {
  const bpmCount = tracks.filter((t) => t.bpm !== null).length;
  const rgTrackCount = tracks.filter((t) => t.replaygain_track_gain).length;
  const albumGain = tracks[0]?.replaygain_album_gain ?? null;
  const albumPeak = tracks[0]?.replaygain_album_peak ?? null;
  const total = tracks.length;

  return (
    <div className="px-4 py-3 space-y-3">
      <AnalysisCard
        icon={<Activity className="w-3.5 h-3.5" />}
        title="BPM"
        subtitle="librosa beat-tracking · 22 kHz mono"
        complete={bpmCount}
        total={total}
        actionLabel={bpmCount === total && total > 0 ? "Re-detect" : "Detect BPM"}
      />
      <AnalysisCard
        icon={<BarChart3 className="w-3.5 h-3.5" />}
        title="ReplayGain"
        subtitle="pyloudnorm · −18 LUFS reference (EBU R128)"
        complete={rgTrackCount}
        total={total}
        actionLabel={rgTrackCount === total && total > 0 ? "Recalculate" : "Calculate ReplayGain"}
        extra={
          albumGain && (
            <div className="grid grid-cols-2 gap-x-4 gap-y-1 mt-2 pt-2 border-t border-line text-xs">
              <KV label="album_gain" value={albumGain} />
              {albumPeak && <KV label="album_peak" value={albumPeak} />}
            </div>
          )
        }
      />
      <p className="text-xs text-fg-4 italic">
        Calculate buttons land in the next milestone. For now this shows what's already in your FLAC tags.
      </p>
    </div>
  );
}

function AnalysisCard({
  icon, title, subtitle, complete, total, actionLabel, extra,
}: {
  icon: React.ReactNode;
  title: string;
  subtitle: string;
  complete: number;
  total: number;
  actionLabel: string;
  extra?: React.ReactNode;
}) {
  const allDone = complete === total && total > 0;
  return (
    <div className="border border-line rounded-md bg-panel-2 p-3">
      <div className="flex items-start gap-2.5">
        <div className="text-fg-3 mt-0.5">{icon}</div>
        <div className="flex-1 min-w-0">
          <div className="font-medium text-md">{title}</div>
          <div className="text-xs text-fg-4 mt-0.5">{subtitle}</div>
          <div className="mt-1.5 mono text-xs tabular-nums">
            <span className={allDone ? "text-ok" : "text-fg-3"}>
              {complete} of {total}
            </span>
            <span className="text-fg-4"> tracks analyzed</span>
          </div>
        </div>
        <button
          type="button"
          className="h-7 px-2.5 border border-line rounded text-xs text-fg-3 cursor-not-allowed opacity-60"
          disabled
          title="Coming in next milestone"
        >
          {actionLabel}
        </button>
      </div>
      {extra}
    </div>
  );
}

function KV({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col">
      <span className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4">{label}</span>
      <span className="mono tabular-nums text-fg-2">{value}</span>
    </div>
  );
}
