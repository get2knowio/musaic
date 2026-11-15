/**
 * Tiny status pill rendered on top of an album cover (Tile) or beside a row
 * (AlbumList) to signal that an AI job exists for that album. Three states:
 *
 *   queued    — Clock,    neutral
 *   running   — Loader,   accent (spinning)
 *   completed — Sparkles, ok-tinted (awaiting review)
 *
 * The badge is purely informational; clicking the album still routes through
 * the normal detail-sheet flow which surfaces the suggestions.
 */
import { CheckCircle2, Clock, Loader2 } from "lucide-react";
import type { Job } from "@/lib/types";
import { useAiStatus } from "@/hooks/useAiStatus";

interface Props {
  job: Job;
  /** Render the larger, drop-shadow variant for tile-cover overlay. Default smaller for list rows. */
  variant?: "tile" | "row";
}

export default function JobStatusBadge({ job, variant = "tile" }: Props) {
  const isTile = variant === "tile";
  // "AI job" only when a model can run.
  const ai = useAiStatus();
  let Icon = Clock;
  let label = "Queued";
  // Tailwind/CSS tokens already in the project: bg-accent, bg-ok, etc.
  let toneClasses = "bg-bg-3/95 text-fg-2 border-line";
  if (job.status === "running") {
    Icon = Loader2;
    label = "Running";
    toneClasses = "bg-accent/90 text-accent-fg border-transparent";
  } else if (job.status === "completed") {
    Icon = CheckCircle2;
    label = "Ready";
    toneClasses = "bg-ok/90 text-white border-transparent";
  }
  return (
    <span
      className={
        "inline-flex items-center gap-1 mono font-semibold uppercase tracking-wider rounded-sm border " +
        toneClasses + " " +
        (isTile
          ? "text-[10px] px-1.5 py-0.5 shadow"
          : "text-[9.5px] px-1 py-0.5")
      }
      title={`${ai.ready ? "AI job" : "Job"}: ${label.toLowerCase()} (${jobTypeShort(job.job_type)})`}
    >
      <Icon className={`${isTile ? "w-2.5 h-2.5" : "w-2.5 h-2.5"} ${job.status === "running" ? "spin" : ""}`} />
      {label}
    </span>
  );
}

function jobTypeShort(t: Job["job_type"]): string {
  if (t === "metadata") return "metadata";
  if (t === "cover") return "cover art";
  return "metadata + cover";
}
