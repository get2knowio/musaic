import { CloudOff, Music } from "lucide-react";
import { Link, useLocation } from "react-router-dom";
import { coverUrl } from "@/lib/api";
import { displayArtist as displayArtistField } from "@/lib/albumFields";
import type { Album, Job } from "@/lib/types";
import JobStatusBadge from "./JobStatusBadge";

interface TileProps {
  album: Album;
  /** Active AI job for this album (queued | running | completed-not-acted-on), if any. */
  activeJob?: Job | null;
}

/**
 * One album tile. Clicking it routes to /library/:id which opens the
 * detail sheet (sheet implementation comes in the next milestone).
 *
 * Missing albums (album.missing_since is set) render greyed-out with
 * a "Missing" pill in the top-right corner. The tile stays clickable
 * so the user can open the DetailSheet to see what we knew, when we
 * last saw it, and either wait for it to come back or remove it from
 * the library explicitly.
 */
export default function Tile({ album, activeJob }: TileProps) {
  const location = useLocation();
  const artist = displayArtist(album);
  const subParts = [
    album.year,
    album.track_count ? `${album.track_count} tr` : "",
  ].filter(Boolean);
  const isMissing = !!album.missing_since;

  return (
    <Link
      // Preserve current ?filter= and ?q= so opening an album doesn't drop
      // the user out of their filtered view when they come back from the sheet.
      to={`/library/${encodeURIComponent(album.id)}${location.search}`}
      className={`group relative flex flex-col overflow-hidden rounded-md border bg-panel transition cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-accent ${
        isMissing
          ? "border-warn/30 hover:border-warn/60"
          : "border-line hover:border-accent hover:-translate-y-0.5 hover:shadow-card"
      }`}
      title={
        isMissing
          ? `Missing since ${formatMissingDate(album.missing_since!)} — ${album.path}`
          : album.path
      }
    >
      <div className={`relative aspect-square bg-bg-2 ${isMissing ? "grayscale opacity-50" : ""}`}>
        <CoverImage album={album} />
        {/* AI-job overlay: tells the user at a glance which albums already
            have a job queued/running/awaiting-review, so they don't double-
            queue while they're triaging the library. Hidden on Missing
            albums — they can't have active jobs against them. */}
        {activeJob && !isMissing && (
          <span className="absolute top-2 right-2">
            <JobStatusBadge job={activeJob} variant="tile" />
          </span>
        )}
        {!album.has_embedded_cover && !album.cover_file && !isMissing && (
          <span
            className="absolute top-2 left-2 mono text-[9.5px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-sm bg-warn-soft text-warn border border-warn/30 shadow"
            title="No cover art"
          >
            no art
          </span>
        )}
      </div>
      {isMissing && (
        // Pill outside the grayscale wrapper so its color stays vivid.
        // Top-right, slightly inset; warn-color palette to signal
        // "not great, needs attention" without screaming red.
        <span
          className="absolute top-2 right-2 inline-flex items-center gap-1 mono text-[10px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-sm bg-warn-soft text-warn border border-warn/40 shadow"
          title={`Album dir not seen on disk since ${formatMissingDate(album.missing_since!)}`}
        >
          <CloudOff className="w-3 h-3" />
          Missing
        </span>
      )}
      <div className={`p-2.5 min-h-[64px] ${isMissing ? "opacity-60" : ""}`}>
        <div className="text-md font-medium truncate" title={album.title ?? album.dir_name}>
          {album.title || album.dir_name}
        </div>
        <div className="text-sm text-fg-3 truncate" title={artist}>
          {artist}
        </div>
        {subParts.length > 0 && (
          <div className="mono text-xs text-fg-4 mt-0.5 tabular-nums">
            {subParts.join(" · ")}
          </div>
        )}
      </div>
    </Link>
  );
}

function CoverImage({ album }: { album: Album }) {
  const hasCover = album.has_embedded_cover || album.cover_file !== null;
  if (!hasCover) {
    return (
      <div className="w-full h-full grid place-items-center text-fg-4">
        <Music className="w-8 h-8" />
      </div>
    );
  }
  return (
    <img
      src={coverUrl(album)}
      alt=""
      loading="lazy"
      className="w-full h-full object-cover"
      onError={(e) => {
        // Cover endpoint returned 404 — replace with placeholder
        e.currentTarget.style.display = "none";
        e.currentTarget.parentElement?.classList.add("cover-failed");
      }}
    />
  );
}

function formatMissingDate(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleDateString();
  } catch {
    return iso;
  }
}

function displayArtist(album: Album): string {
  if (album.is_compilation) return "Various Artists";
  return displayArtistField(album, "Unknown artist");
}
