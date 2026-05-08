import Tile from "./Tile";
import SkeletonTile from "./SkeletonTile";
import { useTileColumns } from "@/hooks/useTileColumns";
import type { Album, Job } from "@/lib/types";

interface TileGridProps {
  albums: Album[];
  /** Show this many skeletons after the real tiles (during a fresh scan). */
  skeletonCount?: number;
  /** Map of album_id → active AI job, used to overlay a status badge. */
  activeJobs?: Map<string, Job>;
}

/**
 * Tile grid with two layout modes:
 *   - **auto** (default): `auto-fill, minmax(--tile-min, 1fr)` — column
 *     count follows viewport width, sized off the `--tile-min` token.
 *   - **fixed**: `repeat(N, 1fr)` driven by `useTileColumns()`. The user
 *     picks a column count from the titlebar; tiles grow/shrink to fill.
 */
export default function TileGrid({ albums, skeletonCount = 0, activeJobs }: TileGridProps) {
  const columns = useTileColumns();
  const gridTemplateColumns =
    columns === null
      ? "repeat(auto-fill, minmax(var(--tile-min), 1fr))"
      : `repeat(${columns}, minmax(0, 1fr))`;

  return (
    <div className="grid gap-4 px-4 py-4" style={{ gridTemplateColumns }}>
      {albums.map((album) => (
        <Tile key={album.id} album={album} activeJob={activeJobs?.get(album.id) ?? null} />
      ))}
      {Array.from({ length: skeletonCount }).map((_, i) => (
        <SkeletonTile key={`skel-${i}`} />
      ))}
    </div>
  );
}
