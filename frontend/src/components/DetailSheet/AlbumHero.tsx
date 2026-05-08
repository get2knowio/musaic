import { useState } from "react";
import { ImagePlus, Music, Tag as TagIcon } from "lucide-react";
import { coverUrl } from "@/lib/api";
import GenreTags from "@/components/GenreTags";
import { useAlbumTags } from "@/hooks/useVocabulary";
import type { Album } from "@/lib/types";
import CoverUploadDialog from "./CoverUploadDialog";
import ResolutionBadge from "./ResolutionBadge";

interface AlbumHeroProps {
  album: Album;
}

/**
 * Top of the detail sheet: cover + title + artist + meta strip + badges.
 * Always visible (never collapsed) since it's the album's identity.
 *
 * The cover thumb doubles as the entry point for manual cover upload —
 * hover/focus reveals an "Replace" overlay that opens
 * `CoverUploadDialog`. Useful when the user has the right artwork
 * (clipboard, file on disk, URL) and Claude can't find it.
 */
export default function AlbumHero({ album }: AlbumHeroProps) {
  const artist = album.is_compilation
    ? "Various Artists"
    : album.album_artist || album.artist || "Unknown artist";
  const hasCover = album.has_embedded_cover || album.cover_file !== null;
  const lengthStr = album.total_seconds ? formatDuration(album.total_seconds) : "—";
  // My Tags chips (album-level intersection only). Hidden when the
  // album has no album-wide tags; partial / per-track tags live in
  // the MyTagsSection below the hero.
  const tagsQuery = useAlbumTags(album.id);
  const myTags = tagsQuery.data?.album_tags ?? [];

  const [uploadOpen, setUploadOpen] = useState(false);

  return (
    <div className="flex gap-4 px-4 py-3.5 border-b border-line">
      <button
        type="button"
        onClick={() => setUploadOpen(true)}
        className="relative w-24 h-24 shrink-0 rounded bg-bg-2 overflow-hidden grid place-items-center text-fg-4 shadow-card group focus:outline-none focus:ring-2 focus:ring-accent"
        title={hasCover ? "Replace cover art" : "Upload cover art"}
        aria-label={hasCover ? "Replace cover art" : "Upload cover art"}
      >
        {hasCover ? (
          <img
            src={coverUrl(album, { size: "full" })}
            alt=""
            className="w-full h-full object-cover"
            onError={(e) => {
              e.currentTarget.style.display = "none";
            }}
          />
        ) : (
          <Music className="w-7 h-7" />
        )}
        <span className="absolute inset-0 bg-black/55 text-white text-[11px] mono uppercase tracking-[0.06em] flex flex-col items-center justify-center gap-1 opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100 transition">
          <ImagePlus className="w-4 h-4" />
          {hasCover ? "Replace" : "Upload"}
        </span>
      </button>
      <CoverUploadDialog
        album={album}
        open={uploadOpen}
        onClose={() => setUploadOpen(false)}
      />
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 mono text-[11px] uppercase tracking-[0.08em] text-fg-4 mb-1">
          <span>Album</span>
          {album.is_compilation && (
            <span className="px-1.5 py-0.5 rounded-sm bg-accent/15 text-accent border border-accent/30 text-[10px] font-semibold">
              Compilation
            </span>
          )}
          <ResolutionBadge albumId={album.id} />
        </div>
        <h2 className="text-lg font-semibold leading-tight truncate" title={album.title || album.dir_name}>
          {album.title || album.dir_name}
        </h2>
        <div className="text-md text-fg-2 truncate mt-0.5" title={artist}>
          {artist}
        </div>
        <div className="mono text-xs text-fg-4 mt-1.5 tabular-nums flex flex-wrap gap-x-3 gap-y-0.5">
          {album.year && <span><b className="text-fg-3 font-medium">{album.year}</b></span>}
          <span><b className="text-fg-3 font-medium">{album.track_count}</b> tracks</span>
          <span>{lengthStr}</span>
        </div>
        {album.genre && (
          <GenreTags genre={album.genre} className="mt-2" />
        )}
        {myTags.length > 0 && (
          <span
            className="inline-flex flex-wrap items-center gap-1 mt-1.5"
            title="Your tags — applied to every track on this album"
          >
            {myTags.map((t) => (
              <span
                key={t}
                className="mono text-[11px] inline-flex items-center gap-1 bg-accent/10 text-accent border border-accent/30 px-2 py-0.5 rounded-full whitespace-nowrap"
              >
                <TagIcon className="w-2.5 h-2.5 opacity-70" />
                {t}
              </span>
            ))}
          </span>
        )}
      </div>
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
