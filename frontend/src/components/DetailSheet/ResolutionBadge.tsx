/**
 * Compact chip showing the album's metadata-resolution status.
 *
 * Drives the "Suggest fixes" button affordance elsewhere in the sheet:
 * green/anchored states mean the deterministic path can produce
 * suggestions for free; amber/unanchored means AI escalation is the
 * only route.
 *
 * Hover shows a plain-language explanation of the chip's state plus
 * the resolver trace (`notes` array) so a curious user can see
 * exactly how Musaic got here without opening any other UI surface.
 */
import { CheckCircle2, Fingerprint, Loader2, Search, ShieldQuestion } from "lucide-react";

import { useAlbumResolution } from "@/hooks/useAlbumResolution";

interface Props {
  albumId: string;
}

export default function ResolutionBadge({ albumId }: Props) {
  const { data: resolution, isLoading, error } = useAlbumResolution(albumId);

  if (isLoading) {
    return (
      <Chip tone="neutral" title="Looking up this album in MusicBrainz and via per-track AcoustID fingerprints…">
        <Loader2 className="w-3 h-3 spin" />
        <span>Resolving…</span>
      </Chip>
    );
  }

  if (error || !resolution) {
    const detail = (error as Error)?.message ?? "Resolution unavailable";
    return (
      <Chip tone="amber" title={withTrace("Couldn't fetch the resolver result for this album.", detail)}>
        <ShieldQuestion className="w-3 h-3" />
        <span>Resolution unavailable</span>
      </Chip>
    );
  }

  const trace = resolution.notes.join("\n") || "No trace available";

  if (resolution.anchor === null) {
    return (
      <Chip
        tone="amber"
        title={withTrace(
          "No confident MusicBrainz release-group match was found from the directory name or per-track fingerprints. Suggesting fixes will require Claude AI.",
          trace,
        )}
      >
        <ShieldQuestion className="w-3 h-3" />
        <span>Unanchored</span>
      </Chip>
    );
  }

  const { anchor_strategy, unresolved_track_count } = resolution;
  const matched = resolution.tracks.length - unresolved_track_count;
  const total = resolution.tracks.length;

  const label =
    anchor_strategy === "fingerprint-vote"
      ? `Anchored • fingerprints${total ? ` (${matched}/${total})` : ""}`
      : anchor_strategy === "dir-search"
      ? "Anchored • directory match"
      : anchor_strategy === "cache"
      ? "Anchored • cached"
      : anchor_strategy === "ai"
      ? "Anchored • AI"
      : "Anchored";

  const explanation =
    anchor_strategy === "fingerprint-vote"
      ? `Matched to a MusicBrainz release group by voting across per-track AcoustID fingerprints (${matched} of ${total} tracks aligned to the canonical tracklist). Suggestions can be generated for free, without Claude AI.`
      : anchor_strategy === "dir-search"
      ? "Matched to a MusicBrainz release group from this album's directory name and total runtime. Suggestions can be generated for free, without Claude AI."
      : anchor_strategy === "cache"
      ? "Matched to a MusicBrainz release group from the local cache (the anchor was discovered on a prior run and saved to the database). Suggestions can be generated for free, without Claude AI."
      : anchor_strategy === "ai"
      ? "Matched to a MusicBrainz release group via Claude AI. Suggestions can be generated for free now that the anchor is known."
      : "Matched to a MusicBrainz release group.";

  const tail =
    unresolved_track_count > 0
      ? ` ${unresolved_track_count} of ${total} tracks couldn't be aligned automatically and may still need AI help.`
      : "";

  const icon =
    anchor_strategy === "fingerprint-vote" ? <Fingerprint className="w-3 h-3" />
    : anchor_strategy === "dir-search" ? <Search className="w-3 h-3" />
    : <CheckCircle2 className="w-3 h-3" />;

  // Confident anchor + every track resolved → green. Anchor present but
  // some tracks didn't align → still green-ish (we have an album anchor
  // and AI can be tightly scoped).
  const tone = unresolved_track_count === 0 ? "green" : "blue";

  return (
    <Chip tone={tone} title={withTrace(explanation + tail, trace)}>
      {icon}
      <span>{label}</span>
    </Chip>
  );
}

function withTrace(explanation: string, trace: string): string {
  return `${explanation}\n\n— Trace —\n${trace}`;
}

function Chip({
  tone,
  title,
  children,
}: {
  tone: "green" | "blue" | "amber" | "neutral";
  title: string;
  children: React.ReactNode;
}) {
  const palette = {
    green:   "bg-emerald-500/10 text-emerald-400 border-emerald-500/30",
    blue:    "bg-accent/10 text-accent border-accent/30",
    amber:   "bg-amber-500/10 text-amber-400 border-amber-500/30",
    neutral: "bg-bg-3 text-fg-3 border-line",
  }[tone];

  return (
    <span
      title={title}
      className={`mono text-[11px] inline-flex items-center gap-1 px-2 py-0.5 rounded-full whitespace-nowrap border ${palette}`}
    >
      {children}
    </span>
  );
}
