import Tile from "./Tile";
import SkeletonTile from "./SkeletonTile";
import type { Album, Job } from "@/lib/types";

interface TileGridProps {
  albums: Album[];
  /** Show this many skeletons after the real tiles (during a fresh scan). */
  skeletonCount?: number;
  /** Map of album_id → active AI job, used to overlay a status badge. */
  activeJobs?: Map<string, Job>;
}

/**
 * Responsive tile grid. Column count adapts to width via auto-fill.
 * Tile minimum size is driven by --tile-min in tokens.css (currently 208px).
 */
export default function TileGrid({ albums, skeletonCount = 0, activeJobs }: TileGridProps) {
  return (
    <div
      className="grid gap-4 px-4 py-4"
      style={{ gridTemplateColumns: "repeat(auto-fill, minmax(var(--tile-min), 1fr))" }}
    >
      {albums.map((album) => (
        <Tile key={album.id} album={album} activeJob={activeJobs?.get(album.id) ?? null} />
      ))}
      {Array.from({ length: skeletonCount }).map((_, i) => (
        <SkeletonTile key={`skel-${i}`} />
      ))}
    </div>
  );
}
