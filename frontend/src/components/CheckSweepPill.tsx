/**
 * Titlebar pill showing the background check sweep's progress.
 *
 * Every health check runs on its own: after startup and after changes to
 * the library, the sweep reruns the checks whose inputs changed. The pill
 * is visible only while it has work (`running`, or `waiting` for a scan or
 * other work to finish first) and hides when it is idle.
 */
import { Activity, Loader2 } from "lucide-react";

import { useCheckSweepStatus } from "@/hooks/useCheckSweepStatus";

export default function CheckSweepPill() {
  const { data } = useCheckSweepStatus();

  if (!data || data.status === "idle") return null;

  const { completed, total } = data;
  const progress = total > 0 ? ` ${Math.min(completed, total)} of ${total}` : "";
  const title =
    data.status === "waiting"
      ? "Health checks are waiting for other work to finish, then continue in the background."
      : "Running health checks in the background on what changed since they last ran.";

  return (
    <span
      role="status"
      title={title}
      className="hidden md:inline-flex items-center gap-1.5 mono text-[11px] px-2 py-0.5 rounded-full bg-accent-soft text-accent border border-accent/30 whitespace-nowrap"
    >
      <Loader2 className="w-3 h-3 spin" />
      <Activity className="w-3 h-3" />
      <span>Checking…{progress}</span>
    </span>
  );
}
