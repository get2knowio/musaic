import { Activity as ActivityIcon, AlertTriangle, Loader2 } from "lucide-react";

import { useActivity } from "@/hooks/useActivity";
import {
  ACTIVITY_PANEL_ID,
  ACTIVITY_TOGGLE_ID,
  toggleActivityPanel,
  useActivityPanel,
} from "@/lib/activityPanel";

/**
 * The toolbar's Activity control: always there, naming the first piece of
 * work in progress ("Scanning · 42 of 180", "+2" for others), or just
 * "Activity". Opens and closes the Activity panel.
 */
export default function ActivityButton() {
  const { toolbar } = useActivity();
  const { open } = useActivityPanel();
  const busy = toolbar.busy > 0;
  const label = busy ? `Activity: ${toolbar.text}${toolbar.stale ? " (out of date)" : ""}` : "Activity";
  return (
    <button
      type="button"
      id={ACTIVITY_TOGGLE_ID}
      onClick={toggleActivityPanel}
      aria-haspopup="dialog"
      aria-expanded={open}
      aria-controls={open ? ACTIVITY_PANEL_ID : undefined}
      aria-label={label}
      title={busy ? label : "Show background activity"}
      data-busy={busy ? "true" : undefined}
      className={
        "inline-flex shrink min-w-0 max-w-[18rem] items-center gap-1.5 h-8 px-2.5 border rounded-md text-sm transition " +
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent " +
        (open
          ? "bg-accent-soft border-accent/40 text-accent"
          : busy
            ? "bg-panel border-accent/40 text-fg hover:bg-bg-3"
            : "bg-panel border-line text-fg-2 hover:bg-bg-3 hover:text-fg")
      }
    >
      {toolbar.stale ? (
        <AlertTriangle className="w-3.5 h-3.5 shrink-0 text-warn" aria-hidden />
      ) : busy ? (
        <Loader2 className="w-3.5 h-3.5 shrink-0 spin text-accent" aria-hidden />
      ) : (
        <ActivityIcon className="w-3.5 h-3.5 shrink-0" aria-hidden />
      )}
      <span className="truncate tabular-nums" data-activity-text>
        {toolbar.text}
      </span>
    </button>
  );
}
