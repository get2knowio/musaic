import { Activity, BarChart3, Loader2, RotateCcw, Sparkles, XCircle } from "lucide-react";
import { toast } from "sonner";

import { useAnalyzeStream, type AnalysisKind } from "@/hooks/useAnalyzeStream";
import type { Track } from "@/lib/types";

interface AudioAnalysisProps {
  albumId: string;
  tracks: Track[];
}

/**
 * Per-album BPM detection (librosa) and ReplayGain calculation
 * (pyloudnorm; EBU R128 −18 LUFS reference).
 *
 * Both kick off SSE jobs against /api/albums/{id}/analyze/stream and
 * stream per-track progress. Only one analysis at a time per album —
 * the second card is disabled while the first is running, and the
 * running card surfaces a live progress bar + last-track value.
 */
export default function AudioAnalysis({ albumId, tracks }: AudioAnalysisProps) {
  const bpmCount = tracks.filter((t) => t.bpm !== null).length;
  const rgTrackCount = tracks.filter((t) => t.replaygain_track_gain).length;
  const albumGain = tracks[0]?.replaygain_album_gain ?? null;
  const albumPeak = tracks[0]?.replaygain_album_peak ?? null;
  const total = tracks.length;

  const analyze = useAnalyzeStream();
  const isRunning = analyze.state.status === "running";
  const runningKind: AnalysisKind | null = isRunning ? analyze.state.what : null;
  // The card a failed run belongs to keeps its state so the error line
  // (the server's message for a refused run) stays visible under it.
  const failedKind: AnalysisKind | null =
    analyze.state.status === "error" ? analyze.state.what : null;
  const progressFor = (kind: AnalysisKind) =>
    runningKind === kind || failedKind === kind ? analyze.state : null;

  const onRun = (what: AnalysisKind) => {
    if (isRunning) return;
    analyze.start(albumId, what).catch((err) => {
      toast.error((err as Error).message || "Analysis failed");
    });
  };

  return (
    <div className="px-4 py-3 space-y-3">
      <AnalysisCard
        icon={<Activity className="w-3.5 h-3.5" />}
        title="BPM"
        subtitle="librosa beat-tracking · 22 kHz mono"
        complete={bpmCount}
        total={total}
        actionLabel={bpmCount === total && total > 0 ? "Re-detect" : "Detect BPM"}
        onAction={() => onRun("bpm")}
        disabled={isRunning && runningKind !== "bpm"}
        running={runningKind === "bpm"}
        progress={progressFor("bpm")}
      />
      <AnalysisCard
        icon={<BarChart3 className="w-3.5 h-3.5" />}
        title="ReplayGain"
        subtitle="pyloudnorm · −18 LUFS reference (EBU R128)"
        complete={rgTrackCount}
        total={total}
        actionLabel={rgTrackCount === total && total > 0 ? "Recalculate" : "Calculate ReplayGain"}
        onAction={() => onRun("replaygain")}
        disabled={isRunning && runningKind !== "replaygain"}
        running={runningKind === "replaygain"}
        progress={progressFor("replaygain")}
        extra={
          albumGain && (
            <div className="grid grid-cols-2 gap-x-4 gap-y-1 mt-2 pt-2 border-t border-line text-xs">
              <KV label="album_gain" value={albumGain} />
              {albumPeak && <KV label="album_peak" value={albumPeak} />}
            </div>
          )
        }
      />
    </div>
  );
}

function AnalysisCard({
  icon, title, subtitle, complete, total, actionLabel, onAction, disabled,
  running, progress, extra,
}: {
  icon: React.ReactNode;
  title: string;
  subtitle: string;
  complete: number;
  total: number;
  actionLabel: string;
  onAction: () => void;
  disabled: boolean;
  running: boolean;
  progress: ReturnType<typeof useAnalyzeStream>["state"] | null;
  extra?: React.ReactNode;
}) {
  const allDone = complete === total && total > 0;
  // While running, prefer the live counter from the SSE stream over the
  // (still-stale) `complete` value sourced from the cache.
  const liveDone = progress?.scannedCount ?? complete;
  const liveTotal = progress?.fileCount ?? total;
  const pct = liveTotal > 0 ? Math.round((liveDone / liveTotal) * 100) : 0;

  // Last visible per-track result — surfaced as a tiny live tail under
  // the progress bar so the user knows the job is making forward progress
  // and roughly what value just landed for the most recent track.
  const lastTail = running && progress ? formatLastTail(progress) : null;

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
            {progress?.errors ? (
              <span className="text-err"> · {progress.errors} error{progress.errors === 1 ? "" : "s"}</span>
            ) : null}
          </div>
        </div>
        <button
          type="button"
          onClick={onAction}
          disabled={disabled || running}
          className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60 inline-flex items-center gap-1.5 shrink-0"
        >
          {running ? (
            <><Loader2 className="w-3 h-3 spin" /> Running…</>
          ) : allDone ? (
            <><RotateCcw className="w-3 h-3" /> {actionLabel}</>
          ) : (
            <><Sparkles className="w-3 h-3" /> {actionLabel}</>
          )}
        </button>
      </div>
      {running && (
        <div className="mt-3 space-y-1.5">
          <div className="flex items-center justify-between text-xs">
            <span className="text-fg-3 tabular-nums mono">{liveDone} / {liveTotal}</span>
            <span className="text-fg-4 tabular-nums mono">{pct}%</span>
          </div>
          <div className="h-1 bg-bg-3 rounded-full overflow-hidden">
            <div className="h-full bg-accent transition-all" style={{ width: `${pct}%` }} />
          </div>
          {lastTail && (
            <div className="text-[11px] text-fg-4 mono truncate" title={lastTail}>
              {lastTail}
            </div>
          )}
        </div>
      )}
      {progress?.status === "error" && (
        <div className="mt-3 text-xs text-err flex items-start gap-1.5" role="alert">
          <XCircle className="w-3 h-3 shrink-0 mt-0.5" />
          <span className="break-words min-w-0">{progress.error ?? "Analysis failed"}</span>
        </div>
      )}
      {extra}
    </div>
  );
}

/** Build the "last track: name → value" tail line for the running card. */
function formatLastTail(state: ReturnType<typeof useAnalyzeStream>["state"]): string | null {
  // Find the most recently-completed file (largest index with status !== "active").
  const entries = Object.values(state.filesByIndex);
  const done = entries.filter((f) => f.status !== "active").sort((a, b) => b.index - a.index);
  const last = done[0];
  if (!last) return null;
  if (last.error) return `${last.filename} · error: ${last.error}`;
  if (last.bpm !== undefined) return `${last.filename} · ${last.bpm.toFixed(1)} BPM`;
  if (last.rgGain !== undefined) {
    return `${last.filename} · ${last.rgGain}${last.rgPeak ? ` · peak ${last.rgPeak}` : ""}`;
  }
  return last.filename;
}

function KV({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex flex-col">
      <span className="mono text-[10.5px] uppercase tracking-[0.06em] text-fg-4">{label}</span>
      <span className="mono tabular-nums text-fg-2">{value}</span>
    </div>
  );
}
