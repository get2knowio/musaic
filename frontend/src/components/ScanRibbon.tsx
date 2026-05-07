import type { ScanState } from "@/hooks/useScanStream";

interface ScanRibbonProps {
  scan: ScanState;
}

/**
 * Sticky ribbon at the top of the main canvas while a scan is in progress.
 * Hidden when scan is idle/done. Once the backend emits a "currently
 * processing" file path, we'll show that here too (per design handoff).
 */
export default function ScanRibbon({ scan }: ScanRibbonProps) {
  if (scan.status !== "scanning" && scan.status !== "loading-cache") return null;

  const pct = scan.total > 0 ? Math.min(100, (scan.scanned / scan.total) * 100) : 0;

  return (
    <div
      className="grid items-center gap-3 px-4 py-2 border-b border-line text-sm shrink-0"
      style={{ gridTemplateColumns: "auto 1fr auto", background: "linear-gradient(to right, var(--accent-soft), transparent 60%)" }}
    >
      <div className="flex items-center gap-2 min-w-[260px]">
        <span className="w-2 h-2 rounded-full bg-accent animate-[pulse_1.6s_infinite]" />
        <span className="font-medium">
          {scan.status === "loading-cache" ? "Loading library…" : "Scanning library…"}
        </span>
        {scan.total > 0 && (
          <span className="mono tabular-nums text-fg-3">
            {scan.scanned} <span className="text-fg-4">of</span> {scan.total}
          </span>
        )}
      </div>

      <div className="grid gap-1.5">
        <div className="grid grid-cols-[1fr_auto] gap-2 items-center">
          <div className="h-1 bg-bg-3 rounded border border-line-2 overflow-hidden">
            <div
              className="h-full bg-accent transition-[width] duration-300 ease-out"
              style={{ width: scan.total > 0 ? `${pct}%` : "30%" }}
            />
          </div>
          {scan.total > 0 && (
            <span className="mono text-xs tabular-nums text-fg-3">
              {pct.toFixed(0)}%
            </span>
          )}
        </div>
        {scan.message && (
          <div className="text-xs text-fg-3 truncate" title={scan.message}>
            {scan.message}
          </div>
        )}
      </div>

      <div className="flex items-center gap-1.5">
        {/* Pause/Cancel placeholders — backend doesn't support them yet */}
        <button className="h-7 px-2.5 border border-line rounded bg-panel text-fg-3 text-xs cursor-not-allowed opacity-60" disabled>
          Cancel
        </button>
      </div>
    </div>
  );
}
