import { useState } from "react";
import { CheckCircle2, Loader2, RotateCcw, TriangleAlert, X } from "lucide-react";
import { useNavigate } from "react-router-dom";

import { useCheckSweepStatus } from "@/hooks/useCheckSweepStatus";
import { itemsText, useReview } from "@/hooks/useReview";
import { clearFirstScan, firstScanMarkedAt } from "@/lib/firstScan";
import { useScanActivity } from "@/lib/scanActivity";

const albums = (n: number) => `${n} ${n === 1 ? "album" : "albums"}`;

/**
 * What the first scan of a newly chosen folder came to (`lib/firstScan.ts`),
 * above the grid until dismissed:
 *
 *  - done: the album count, whether checks are still running, and Review
 *    findings once there is something to review (a folder with no album
 *    shows the Library's own empty state instead);
 *  - cancelled / aborted / failed: that, in so many words (nothing was
 *    saved), with Scan again; never "complete".
 *
 * Shown nothing while the scan runs (its progress is in Activity). The
 * outcome is read from the app-wide scan record (`lib/scanActivity.ts`),
 * so a first scan that ended while another page was open is still
 * reported, and a scan whose stream was lost ("lost") is not reported as
 * ended either way.
 */
export default function FirstScanReport({
  albumCount,
  onScanAgain,
}: {
  albumCount: number;
  onScanAgain: () => void;
}) {
  const navigate = useNavigate();
  const scan = useScanActivity();
  const [, setDismissed] = useState(0);
  const sweep = useCheckSweepStatus();
  const review = useReview();

  const ended =
    scan.phase === "done" || scan.phase === "cancelled" || scan.phase === "aborted" || scan.phase === "error";
  const markedAt = firstScanMarkedAt();
  // An earlier scan's outcome (before this folder was chosen) is not this one's.
  if (!ended || markedAt === null || (scan.endedAt ?? 0) < markedAt) return null;
  if (scan.phase === "done" && albumCount === 0) return null;

  const dismiss = () => {
    clearFirstScan();
    setDismissed((n) => n + 1);
  };

  let icon = <CheckCircle2 className="w-4 h-4 text-ok shrink-0 mt-0.5" />;
  let headline: string;
  let detail: string | null = null;
  let action: React.ReactNode = null;

  if (scan.phase === "done") {
    headline = `Scan complete: ${albums(albumCount)} in your library.`;
    const checking = sweep.data?.status === "running" || sweep.data?.status === "waiting";
    if (checking) {
      icon = <Loader2 className="w-4 h-4 text-accent shrink-0 mt-0.5 animate-spin" />;
      detail = "Checks are running in the background; what they find is listed in Review.";
    } else if (review.count > 0) {
      detail = `Checks found ${itemsText(review.count, review.countIsLowerBound)} to review.`;
    } else if (sweep.data?.status === "idle" && review.allClear) {
      detail = "Checks found nothing to review.";
    }
    if (review.count > 0) {
      action = (
        <button
          type="button"
          onClick={() => {
            clearFirstScan();
            navigate("/review");
          }}
          className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition whitespace-nowrap"
        >
          Review findings
        </button>
      );
    }
  } else {
    icon = <TriangleAlert className="w-4 h-4 text-warn shrink-0 mt-0.5" />;
    headline =
      scan.phase === "cancelled"
        ? "Scan cancelled. Nothing was saved."
        : scan.phase === "aborted"
          ? "Scan stopped without saving."
          : "The scan failed.";
    const kept = albumCount > 0 ? ` The library still shows its previous scan (${albums(albumCount)}).` : "";
    // A cancel's message only repeats the headline; an abort or failure says why.
    const why = scan.phase !== "cancelled" && scan.message ? scan.message.replace(/\.?\s*$/, ".") : "";
    detail = `${why}${kept}`.trim() || null;
    action = (
      <button
        type="button"
        onClick={onScanAgain}
        className="h-8 px-3 rounded bg-accent text-accent-fg text-sm font-medium hover:opacity-90 transition whitespace-nowrap inline-flex items-center gap-1.5"
      >
        <RotateCcw className="w-3.5 h-3.5" />
        Scan again
      </button>
    );
  }

  return (
    <div
      role="status"
      aria-label="First scan"
      data-first-scan={scan.phase}
      className="flex flex-wrap items-start gap-x-3 gap-y-2 px-4 py-2.5 border-b border-line bg-panel-2"
    >
      {icon}
      <p className="flex-1 min-w-[12rem] text-sm text-fg-2 leading-relaxed">
        <span className="font-medium text-fg">{headline}</span>
        {detail && <> {detail}</>}
      </p>
      <div className="flex items-center gap-1.5 ml-auto">
        {action}
        <button
          type="button"
          aria-label="Dismiss"
          title="Dismiss"
          onClick={dismiss}
          className="h-8 w-8 grid place-items-center rounded text-fg-3 hover:bg-bg-3 transition"
        >
          <X className="w-4 h-4" />
        </button>
      </div>
    </div>
  );
}
