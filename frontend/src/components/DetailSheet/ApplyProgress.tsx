import { CheckCircle2, Download, XCircle } from "lucide-react";
import type { ApplyState } from "@/hooks/useApplyStream";

interface ApplyProgressProps {
  state: ApplyState;
}

/**
 * Top-of-body banner during apply: shows the cover-download phase, then
 * the per-file write phase with overall progress bar. The track-list
 * spinner column is rendered separately by TrackList using state.filesByIndex.
 */
export default function ApplyProgress({ state }: ApplyProgressProps) {
  if (state.status === "idle") return null;

  return (
    <div className="px-4 py-3 border-b border-line bg-panel-2 space-y-2">
      {/* Cover download phase */}
      {state.coverUrl && (
        <div className="flex items-center gap-2 text-xs text-fg-2">
          <Download className="w-3.5 h-3.5 text-accent" />
          <span>Downloading cover from <code className="mono text-fg-3 break-all">{abbreviateUrl(state.coverUrl)}</code>…</span>
        </div>
      )}

      {/* Writing phase */}
      {(state.status === "writing" || state.status === "done") && state.fileCount > 0 && (
        <>
          <div className="flex items-center justify-between text-xs">
            <span className="text-fg-2">
              {state.status === "writing" ? "Writing FLAC tags…" : "Apply complete"}{" "}
              <span className="mono tabular-nums text-fg-3">
                {state.scannedCount} <span className="text-fg-4">of</span> {state.fileCount}
              </span>
            </span>
            <span className="mono tabular-nums text-fg-4">
              {Math.round((state.scannedCount / state.fileCount) * 100)}%
            </span>
          </div>
          <div className="h-1 bg-bg-3 rounded border border-line-2 overflow-hidden">
            <div
              className="h-full bg-accent transition-[width] duration-200 ease-out"
              style={{ width: `${(state.scannedCount / state.fileCount) * 100}%` }}
            />
          </div>
        </>
      )}

      {state.status === "done" && (
        <div className="flex items-center gap-2 text-sm text-ok">
          <CheckCircle2 className="w-4 h-4" />
          <span>
            {state.savedCount > 0
              ? `Wrote ${state.savedCount} file${state.savedCount === 1 ? "" : "s"}`
              : "No changes needed"}
            {state.artChanged && " · cover art embedded"}
            {state.errors > 0 && (
              <span className="text-err"> · {state.errors} error{state.errors === 1 ? "" : "s"}</span>
            )}
          </span>
        </div>
      )}

      {state.status === "error" && state.error && (
        <div className="flex items-center gap-2 text-sm text-err">
          <XCircle className="w-4 h-4" />
          <span>{state.error}</span>
        </div>
      )}
    </div>
  );
}

function abbreviateUrl(url: string): string {
  try {
    const u = new URL(url);
    return u.host;
  } catch {
    return url.length > 60 ? url.slice(0, 57) + "…" : url;
  }
}
