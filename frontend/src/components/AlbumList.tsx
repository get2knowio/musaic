import { Music } from "lucide-react";
import { Link, useLocation, useParams } from "react-router-dom";

import { coverUrl } from "@/lib/api";
import { displayArtist } from "@/lib/albumFields";
import type { Album, Job } from "@/lib/types";
import JobStatusBadge from "./JobStatusBadge";
import { requestPanelFocus } from "./DetailSheet/panelFocus";

interface AlbumListProps {
  albums: Album[];
  /** Show this many skeleton rows after the real ones (during a fresh scan). */
  skeletonCount?: number;
  /** Map of album_id → active AI job, used to render a status pill in the row. */
  activeJobs?: Map<string, Job>;
}

/**
 * Dense list view of the library. Same data as TileGrid, columnar layout.
 * Click a row → shows the album in the panel beside the list (URL preserves
 * current ?filter= / ?q= so the user returns to the same filtered view on
 * close). Same push/replace and keyboard-focus behaviour as Tile.
 */
export default function AlbumList({ albums, skeletonCount = 0, activeJobs }: AlbumListProps) {
  return (
    <div className="px-4 py-4">
      <div className="rounded-md border border-line bg-panel-2 overflow-hidden">
        <ListHeader />
        {albums.map((album) => (
          <Row key={album.id} album={album} activeJob={activeJobs?.get(album.id) ?? null} />
        ))}
        {Array.from({ length: skeletonCount }).map((_, i) => (
          <SkeletonRow key={`skel-${i}`} />
        ))}
      </div>
    </div>
  );
}

// Same column template used by header + rows so they line up.
const ROW_TEMPLATE = "40px minmax(0, 2fr) minmax(0, 1.4fr) 64px 64px 80px";

function ListHeader() {
  return (
    <div
      className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 mono text-xs uppercase tracking-[0.06em]"
      style={{ gridTemplateColumns: ROW_TEMPLATE }}
    >
      <span />
      <span>Album</span>
      <span>Artist</span>
      <span className="text-right">Year</span>
      <span className="text-right">Tracks</span>
      <span className="text-right">Length</span>
    </div>
  );
}

function Row({ album, activeJob }: { album: Album; activeJob: Job | null }) {
  const location = useLocation();
  const { albumId: openAlbumId } = useParams<{ albumId: string }>();
  const isOpen = openAlbumId === album.id;
  const artist = album.is_compilation
    ? "Various Artists"
    : displayArtist(album, "Unknown artist");
  const hasCover = album.has_embedded_cover || album.cover_file !== null;

  return (
    <Link
      to={`/library/${encodeURIComponent(album.id)}${location.search}`}
      replace={!!openAlbumId}
      state={location.state}
      onClick={(e) => {
        // detail 0: activated from the keyboard, not a pointer.
        if (e.detail === 0) requestPanelFocus(album.id);
      }}
      data-album-id={album.id}
      aria-current={isOpen ? "true" : undefined}
      className={`grid gap-3 px-3 py-1.5 border-b border-line last:border-0 items-center text-sm transition cursor-pointer focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent ${
        isOpen ? "bg-accent-soft ring-2 ring-inset ring-accent" : "hover:bg-bg-3"
      }`}
      style={{ gridTemplateColumns: ROW_TEMPLATE }}
      title={album.path}
    >
      <div className="w-8 h-8 rounded bg-bg-2 overflow-hidden grid place-items-center text-fg-4">
        {hasCover ? (
          <img
            src={coverUrl(album)}
            alt=""
            loading="lazy"
            className="w-full h-full object-cover"
            onError={(e) => { e.currentTarget.style.display = "none"; }}
          />
        ) : (
          <Music className="w-3.5 h-3.5" />
        )}
      </div>
      <div className="min-w-0 flex items-center gap-2">
        <span className="font-medium truncate" title={album.title || album.dir_name}>
          {album.title || album.dir_name}
        </span>
        {activeJob && <JobStatusBadge job={activeJob} variant="row" />}
        {!hasCover && (
          <span className="mono text-[10px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-sm bg-warn-soft text-warn border border-warn/30 shrink-0" title="No cover art">
            no art
          </span>
        )}
      </div>
      <span className="truncate text-fg-2" title={artist}>
        {artist}
      </span>
      <span className="mono tabular-nums text-fg-3 text-right">{album.year || "—"}</span>
      <span className="mono tabular-nums text-fg-3 text-right">{album.track_count || "—"}</span>
      <span className="mono tabular-nums text-fg-3 text-right">
        {album.total_seconds ? formatDuration(album.total_seconds) : "—"}
      </span>
    </Link>
  );
}

function SkeletonRow() {
  return (
    <div
      className="grid gap-3 px-3 py-1.5 border-b border-line last:border-0 items-center"
      style={{ gridTemplateColumns: ROW_TEMPLATE }}
    >
      <div className="w-8 h-8 rounded bg-bg-2 animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      <div className="h-3.5 bg-bg-2 rounded animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      <div className="h-3 bg-bg-2 rounded animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      <div />
      <div />
      <div />
    </div>
  );
}

function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (h > 0) return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
  return `${m}:${s.toString().padStart(2, "0")}`;
}
