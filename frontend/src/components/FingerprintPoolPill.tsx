/**
 * Titlebar pill showing the eager fingerprinting pool's progress.
 *
 * Visible only while `status === "running"`. Other states (idle, done,
 * cancelled, unavailable) hide the pill — the resolver picks up the
 * benefit silently from then on, no need to nag the user.
 *
 * The hook polls every 2s while running, every 30s otherwise.
 */
import { Fingerprint, Loader2 } from "lucide-react";

import { useFingerprintPoolStatus } from "@/hooks/useFingerprintPoolStatus";

export default function FingerprintPoolPill() {
  const { data } = useFingerprintPoolStatus();

  if (!data || data.status !== "running") return null;

  const { completed, total, errors } = data;
  const totalLabel = total > 0 ? `${completed}/${total}` : completed;
  const errLabel = errors > 0 ? ` · ${errors} skipped` : "";

  return (
    <span
      title={`Computing AcoustID fingerprints in the background — anchors albums whose folder names don't match MusicBrainz. ${completed}/${total || "?"} done${errLabel}.`}
      className="hidden md:inline-flex items-center gap-1.5 mono text-[11px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-400 border border-emerald-500/30 whitespace-nowrap"
    >
      <Loader2 className="w-3 h-3 spin" />
      <Fingerprint className="w-3 h-3" />
      <span>Fingerprinting {totalLabel}{errLabel}</span>
    </span>
  );
}
