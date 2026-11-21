import { CloudOff, Music } from "lucide-react";
import { Link, useLocation, useParams } from "react-router-dom";

import { coverUrl } from "@/lib/api";
import { displayArtist } from "@/lib/albumFields";
import { listLayout, type ListLayout } from "@/lib/albumListLayout";
import type { Album, Job } from "@/lib/types";
import { useElementWidth } from "@/hooks/useElementWidth";
import JobStatusBadge from "./JobStatusBadge";
import { requestPanelFocus } from "./DetailSheet/panelFocus";

interface AlbumListProps {
  albums: Album[];
  /** Show this many skeleton rows after the real ones (during a fresh scan). */
  skeletonCount?: number;
  /** Map of album_id → active AI job, used to render a status pill in the row. */
  activeJobs?: Map<string, Job>;
}

/** The grid columns for a layout; header, rows and skeletons share it. */
function template(l: ListLayout): string {
  return [
    "40px",
    l.stacked ? "minmax(0, 1fr)" : "minmax(0, 2fr) minmax(0, 1.4fr)",
    l.year ? "56px" : "",
    l.tracks ? "56px" : "",
    l.length ? "72px" : "",
  ]
    .filter(Boolean)
    .join(" ");
}

/**
 * Dense list view of the library. Same data as TileGrid, columnar layout.
 * Click a row → shows the album in the panel beside the list (URL preserves
 * current ?filter= / ?q= so the user returns to the same filtered view on
 * close). Same push/replace and keyboard-focus behaviour as Tile.
 */
export default function AlbumList({ albums, skeletonCount = 0, activeJobs }: AlbumListProps) {
  const [ref, width] = useElementWidth<HTMLDivElement>();
  const layout = listLayout(width);
  return (
    <div ref={ref} className="px-4 py-4" data-list-layout={layoutName(layout)}>
      <div className="rounded-md border border-line bg-panel-2 overflow-hidden">
        <ListHeader layout={layout} />
        {albums.map((album) => (
          <Row key={album.id} album={album} activeJob={activeJobs?.get(album.id) ?? null} layout={layout} />
        ))}
        {Array.from({ length: skeletonCount }).map((_, i) => (
          <SkeletonRow key={`skel-${i}`} layout={layout} />
        ))}
      </div>
    </div>
  );
}

function layoutName(l: ListLayout): string {
  const cols = [l.year && "year", l.tracks && "tracks", l.length && "length"].filter(Boolean);
  return `${l.stacked ? "stacked" : "columns"}${cols.length ? `+${cols.join("+")}` : ""}`;
}

function ListHeader({ layout }: { layout: ListLayout }) {
  return (
    <div
      className="grid gap-3 px-3 py-2 border-b border-line text-fg-4 mono text-xs uppercase tracking-[0.06em]"
      style={{ gridTemplateColumns: template(layout) }}
    >
      <span />
      {layout.stacked ? (
        <span>Album · Artist</span>
      ) : (
        <>
          <span>Album</span>
          <span>Artist</span>
        </>
      )}
      {layout.year && <span className="text-right">Year</span>}
      {layout.tracks && <span className="text-right">Tracks</span>}
      {layout.length && <span className="text-right">Length</span>}
    </div>
  );
}

function Row({ album, activeJob, layout }: { album: Album; activeJob: Job | null; layout: ListLayout }) {
  const location = useLocation();
  const { albumId: openAlbumId } = useParams<{ albumId: string }>();
  const isOpen = openAlbumId === album.id;
  const artist = album.is_compilation
    ? "Various Artists"
    : displayArtist(album, "Unknown artist");
  const hasCover = album.has_embedded_cover || album.cover_file !== null;
  const isMissing = !!album.missing_since;
  const title = album.title || album.dir_name;

  const badges = (
    <>
      {isMissing && <MissingBadge since={album.missing_since!} />}
      {activeJob && !isMissing && <JobStatusBadge job={activeJob} variant="row" />}
      {!hasCover && !isMissing && (
        <span className="mono text-[10px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-sm bg-warn-soft text-warn border border-warn/30 shrink-0" title="No cover art">
          no art
        </span>
      )}
    </>
  );

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
      style={{ gridTemplateColumns: template(layout) }}
      title={isMissing ? `Folder missing since ${formatMissingDate(album.missing_since!)} — ${album.path}` : album.path}
    >
      <div
        className={`w-8 h-8 rounded bg-bg-2 overflow-hidden grid place-items-center text-fg-4 ${
          isMissing ? "grayscale opacity-50" : ""
        }`}
      >
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
      {layout.stacked ? (
        <div className="min-w-0">
          <div className="flex items-center gap-2 min-w-0">
            <span className="font-medium truncate" data-album-title title={title}>{title}</span>
          </div>
          <div className="flex items-center gap-2 min-w-0">
            <span className="truncate text-fg-3 text-xs" data-album-artist title={artist}>{artist}</span>
            {badges}
          </div>
        </div>
      ) : (
        <>
          <div className="min-w-0 flex items-center gap-2">
            <span className="font-medium truncate" data-album-title title={title}>{title}</span>
            {badges}
          </div>
          <span className="truncate text-fg-2" data-album-artist title={artist}>{artist}</span>
        </>
      )}
      {layout.year && <span className="mono tabular-nums text-fg-3 text-right">{album.year || "—"}</span>}
      {layout.tracks && <span className="mono tabular-nums text-fg-3 text-right">{album.track_count || "—"}</span>}
      {layout.length && (
        <span className="mono tabular-nums text-fg-3 text-right">
          {album.total_seconds ? formatDuration(album.total_seconds) : "—"}
        </span>
      )}
    </Link>
  );
}

/** The folder is not on disk: said in words (not colour or a tooltip alone). */
function MissingBadge({ since }: { since: string }) {
  return (
    <span
      data-missing-folder
      className="inline-flex items-center gap-1 mono text-[10px] font-semibold uppercase tracking-wider px-1.5 py-0.5 rounded-sm bg-warn-soft text-warn border border-warn/40 shrink-0"
    >
      <CloudOff className="w-3 h-3" aria-hidden />
      Folder missing
      <span className="sr-only">, not seen on disk since {formatMissingDate(since)}</span>
    </span>
  );
}

function SkeletonRow({ layout }: { layout: ListLayout }) {
  return (
    <div
      className="grid gap-3 px-3 py-1.5 border-b border-line last:border-0 items-center"
      style={{ gridTemplateColumns: template(layout) }}
    >
      <div className="w-8 h-8 rounded bg-bg-2 animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      <div className="h-3.5 bg-bg-2 rounded animate-[skel-pulse_1.5s_ease-in-out_infinite]" />
      {!layout.stacked && <div className="h-3 bg-bg-2 rounded animate-[skel-pulse_1.5s_ease-in-out_infinite]" />}
      {layout.year && <div />}
      {layout.tracks && <div />}
      {layout.length && <div />}
    </div>
  );
}

function formatMissingDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleDateString();
}

function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (h > 0) return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
  return `${m}:${s.toString().padStart(2, "0")}`;
}
