import { Music } from "lucide-react";
import { Link, useLocation } from "react-router-dom";
import { coverUrl } from "@/lib/api";
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
 */
export default function Tile({ album, activeJob }: TileProps) {
  const location = useLocation();
  const artist = displayArtist(album);
  const subParts = [
    album.year,
    album.track_count ? `${album.track_count} tr` : "",
  ].filter(Boolean);

  return (
    <Link
      // Preserve current ?filter= and ?q= so opening an album doesn't drop
      // the user out of their filtered view when they come back from the sheet.
      to={`/library/${encodeURIComponent(album.id)}${location.search}`}
      className="group relative flex flex-col overflow-hidden rounded-md border border-line bg-panel transition cursor-pointer hover:border-accent hover:-translate-y-0.5 hover:shadow-card focus:outline-none focus-visible:ring-2 focus-visible:ring-accent"
      title={album.path}
    >
      <div className="relative aspect-square bg-bg-2">
        <CoverImage album={album} />
        {/* AI-job overlay: tells the user at a glance which albums already
            have a job queued/running/awaiting-review, so they don't double-
            queue while they're triaging the library. */}
        {activeJob && (
          <span className="absolute top-2 right-2">
            <JobStatusBadge job={activeJob} variant="tile" />
          </span>
        )}
        {!album.has_embedded_cover && !album.cover_file && (
          <span
            className="absolute top-2 left-2 mono text-[9.5px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-sm bg-warn-soft text-warn border border-warn/30 shadow"
            title="No cover art"
          >
            no art
          </span>
        )}
      </div>
      <div className="p-2.5 min-h-[64px]">
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

function displayArtist(album: Album): string {
  if (album.is_compilation) return "Various Artists";
  return album.album_artist || album.artist || "Unknown artist";
}
