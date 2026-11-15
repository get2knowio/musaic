/**
 * Temporary "Check now" action for the Health page.
 *
 * Most checks run during every library scan. The few that do not
 * (`runs_during_scan: false` in their meta, today the tag and filename
 * comparisons) have no other trigger until checks run automatically in the
 * background. This button runs just those, one after another, so they stay
 * reachable in the meantime.
 *
 * Self-contained on purpose: when automatic checks land, delete this file
 * and its one use in `pages/Health.tsx`; nothing else depends on it.
 */
import { useState } from "react";
import { Loader2, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { runCheck } from "@/lib/api";
import type { CheckSummary } from "@/lib/types";

export default function CheckNowButton({ checks }: { checks: CheckSummary[] }) {
  const onDemand = checks.filter((c) => !c.meta.runs_during_scan);
  // 0 = idle; otherwise the 1-based index of the check being run.
  const [current, setCurrent] = useState(0);
  const busy = current > 0;
  // A run started elsewhere (or before a reload) still shows as running
  // in the checks list; don't start a second one on top of it.
  const alreadyRunning = onDemand.some((c) => c.last_run?.status === "running");

  if (onDemand.length === 0) return null;

  const onClick = async () => {
    if (busy) return;
    let failed = 0;
    // One at a time: these checks query MusicBrainz, which is paced to one
    // request per second anyway, and the run request returns when the run
    // ends, which is what drives the progress label.
    for (let i = 0; i < onDemand.length; i++) {
      setCurrent(i + 1);
      try {
        await runCheck(onDemand[i].meta.id, { scope: "all" });
      } catch (err) {
        failed++;
        toast.error(`${onDemand[i].meta.label}: ${(err as Error).message || "couldn't run"}`);
      }
    }
    setCurrent(0);
    // Findings and counts refresh through the checks stream; this only
    // confirms the button finished.
    if (failed === 0) toast.success("Checks finished", { duration: 3000 });
  };

  const names = onDemand.map((c) => c.meta.label).join(", ");
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy || alreadyRunning}
      title={`Most checks run with every library scan. This runs the rest now: ${names}.`}
      className="h-7 px-2.5 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 transition disabled:opacity-60 inline-flex items-center gap-1.5"
    >
      {busy ? (
        <>
          <Loader2 className="w-3 h-3 spin" />
          Checking {current} of {onDemand.length}…
        </>
      ) : alreadyRunning ? (
        <>
          <Loader2 className="w-3 h-3 spin" />
          Checking…
        </>
      ) : (
        <>
          <RefreshCw className="w-3 h-3" />
          Check now
        </>
      )}
    </button>
  );
}
