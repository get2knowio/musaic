/**
 * Compact chip showing the album's metadata-resolution status.
 *
 * Green/anchored states mean the resolver can propose MusicBrainz
 * corrections in the panel; amber/unanchored means it cannot.
 *
 * Hover shows a plain-language explanation of the chip's state plus
 * the resolver trace (`notes` array) so a curious user can see
 * exactly how Musaic got here without opening any other UI surface.
 */
import { CheckCircle2, CircleSlash, Loader2, ShieldQuestion } from "lucide-react";

import { useAlbumResolution } from "@/hooks/useAlbumResolution";

interface Props {
  albumId: string;
}

export default function ResolutionBadge({ albumId }: Props) {
  const { data: resolution, isLoading, isFetching, error, refetch } = useAlbumResolution(albumId);

  if (isLoading) {
    return (
      <Chip tone="neutral" title="Looking up this album in MusicBrainz and via per-track AcoustID fingerprints…">
        <Loader2 className="w-3 h-3 spin" />
        <span>Resolving</span>
      </Chip>
    );
  }

  if (error || !resolution) {
    // The lookup didn't answer (409 while a database restore runs, or a
    // transport failure). That says nothing about whether the album is
    // identified, so don't claim "Not identified": say the lookup is
    // unavailable, give the server's reason, and let a click retry it.
    const detail = (error as Error)?.message ?? "Resolution unavailable";
    const busy = (error as { status?: number } | null)?.status === 409;
    return (
      <button
        type="button"
        onClick={() => { void refetch(); }}
        disabled={isFetching}
        title={`${busy ? "Musaic is busy and couldn't look this album up." : "Couldn't fetch the resolver result for this album."}\n${detail}\n\nClick to try again.`}
        className="mono text-[11px] inline-flex items-center gap-1 px-2 py-0.5 rounded-full whitespace-nowrap border bg-bg-3 text-fg-3 border-line hover:text-fg transition disabled:opacity-60"
      >
        {isFetching ? <Loader2 className="w-3 h-3 spin" /> : <CircleSlash className="w-3 h-3" />}
        <span>{busy ? "Lookup busy" : "Lookup failed"}</span>
      </button>
    );
  }

  const trace = resolution.notes.join("\n") || "No trace available";

  if (resolution.anchor === null) {
    return (
      <Chip
        tone="amber"
        title={withTrace(
          "No confident MusicBrainz release-group match was found from the directory name or per-track fingerprints.",
          trace,
        )}
      >
        <ShieldQuestion className="w-3 h-3" />
        <span>Not identified</span>
      </Chip>
    );
  }

  const { anchor_strategy, unresolved_track_count } = resolution;
  const matched = resolution.tracks.length - unresolved_track_count;
  const total = resolution.tracks.length;

  const strategyDetail =
    anchor_strategy === "fingerprint-vote"
      ? `Matched via per-track AcoustID fingerprints (${matched} of ${total} tracks aligned to the canonical tracklist).`
      : anchor_strategy === "dir-search"
      ? "Matched from this album's directory name and total runtime."
      : anchor_strategy === "cache"
      ? "Matched from the local cache (anchor was discovered on a prior run)."
      : anchor_strategy === "ai"
      ? "Matched by an earlier lookup; the anchor is cached for free reuse."
      : "Matched to a MusicBrainz release group.";

  // Confident anchor + every track resolved → green. Anchor present but
  // some tracks didn't align → blue (an album anchor, but corrections
  // need every track aligned).
  const tone = unresolved_track_count === 0 ? "green" : "blue";

  return (
    <Chip
      tone={tone}
      title={withTrace(
        `${strategyDetail}${unresolved_track_count === 0 ? " Corrections can be generated for free from MusicBrainz." : ""}`,
        trace,
      )}
    >
      <CheckCircle2 className="w-3 h-3" />
      <span>Identified</span>
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
