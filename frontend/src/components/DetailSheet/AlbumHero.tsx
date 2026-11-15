import { useRef, useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { ChevronDown, ImagePlus, Music, Sparkles, Tag as TagIcon, Upload } from "lucide-react";
import { coverUrl } from "@/lib/api";
import GenreTags from "@/components/GenreTags";
import { useAlbumTags } from "@/hooks/useVocabulary";
import { displayArtist } from "@/lib/albumFields";
import type { Album } from "@/lib/types";
import CoverUploadDialog from "./CoverUploadDialog";
import ResolutionBadge from "./ResolutionBadge";

interface AlbumHeroProps {
  album: Album;
  /** Show the MusicBrainz cover as a row in the corrections, to preview
   *  and apply there. Omitted when MusicBrainz has no cover to offer or it
   *  is already proposed. */
  onUseMusicBrainzCover?: () => void;
  /** Queue an AI cover search. Omitted unless AI is ready
   *  (lib/aiGating.ts) and a job can be queued. */
  onAskAiForCover?: () => void;
}

/**
 * Top of the album panel: cover + title + artist + meta strip + badges.
 * Always visible (never collapsed) since it's the album's identity.
 *
 * The one Cover control lives here: a visible button whose menu offers
 * the MusicBrainz cover (previewed and applied in the corrections),
 * upload / paste / URL (`CoverUploadDialog`) and, when AI is ready, an AI
 * search. A Missing album has no Cover control: its files cannot be
 * written.
 */
export default function AlbumHero({ album, onUseMusicBrainzCover, onAskAiForCover }: AlbumHeroProps) {
  const artist = album.is_compilation
    ? "Various Artists"
    : displayArtist(album, "Unknown artist");
  const hasCover = album.has_embedded_cover || album.cover_file !== null;
  const lengthStr = album.total_seconds ? formatDuration(album.total_seconds) : "—";
  // My Tags chips (album-level intersection only). Hidden when the
  // album has no album-wide tags; partial / per-track tags live in
  // the MyTagsSection below the hero.
  const tagsQuery = useAlbumTags(album.id);
  const myTags = tagsQuery.data?.album_tags ?? [];

  const [uploadOpen, setUploadOpen] = useState(false);
  // Missing: a cover change is an apply (refused with 409), so there is no
  // Cover control. The resolution chip is left out too: the resolver reads
  // the tracks from disk, and it only steers the actions a Missing album
  // does not offer.
  const isMissing = !!album.missing_since;

  return (
    <div className="flex gap-4 px-4 py-3.5 border-b border-line">
      <div className="shrink-0 flex flex-col items-center gap-1.5">
        <div className="relative w-24 h-24 rounded bg-bg-2 overflow-hidden grid place-items-center text-fg-4 shadow-card">
          {hasCover ? (
            <img
              // Missing: the full-size cover is read from the files, so
              // show the stored thumbnail (kept until the album is removed).
              src={isMissing ? coverUrl(album) : coverUrl(album, { size: "full" })}
              alt=""
              className="w-full h-full object-cover"
              onError={(e) => {
                e.currentTarget.style.display = "none";
              }}
            />
          ) : (
            <Music className="w-7 h-7" />
          )}
        </div>
        {!isMissing && (
          <CoverMenu
            hasCover={hasCover}
            onUpload={() => setUploadOpen(true)}
            onUseMusicBrainzCover={onUseMusicBrainzCover}
            onAskAiForCover={onAskAiForCover}
          />
        )}
      </div>
      {!isMissing && (
        <CoverUploadDialog
          album={album}
          open={uploadOpen}
          onClose={() => setUploadOpen(false)}
        />
      )}
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2 mono text-[11px] uppercase tracking-[0.08em] text-fg-4 mb-1">
          <span>Album</span>
          {album.is_compilation && (
            <span className="px-1.5 py-0.5 rounded-sm bg-accent/15 text-accent border border-accent/30 text-[10px] font-semibold">
              Compilation
            </span>
          )}
          {!isMissing && <ResolutionBadge albumId={album.id} />}
        </div>
        {/* Not a heading: the panel's header carries the album title as
            its h2 (the panel's label and keyboard focus target). */}
        <p className="text-lg font-semibold leading-tight truncate" title={album.title || album.dir_name}>
          {album.title || album.dir_name}
        </p>
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

/**
 * "Add cover" / "Change cover", by whether the album has one. Always a
 * menu, so the button means the same thing whatever is on offer.
 */
function CoverMenu({
  hasCover,
  onUpload,
  onUseMusicBrainzCover,
  onAskAiForCover,
}: {
  hasCover: boolean;
  onUpload: () => void;
  onUseMusicBrainzCover?: () => void;
  onAskAiForCover?: () => void;
}) {
  const item =
    "flex items-center gap-2 px-2.5 h-8 rounded text-sm text-fg-2 outline-none cursor-pointer data-[highlighted]:bg-bg-3 data-[highlighted]:text-fg";
  // An item's action runs once the menu has closed and focus is back on
  // the button. A dialog opened earlier would lose focus to the menu's own
  // focus return, and would not know where to return it when it closes.
  const trigger = useRef<HTMLButtonElement>(null);
  const pending = useRef<(() => void) | null>(null);
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          ref={trigger}
          type="button"
          className="h-7 px-2 border border-line rounded text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition inline-flex items-center gap-1 whitespace-nowrap"
        >
          <ImagePlus className="w-3.5 h-3.5" />
          {hasCover ? "Change cover" : "Add cover"}
          <ChevronDown className="w-3 h-3 opacity-70" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="start"
          sideOffset={4}
          className="min-w-56 rounded-md border border-line-2 bg-panel shadow-popup p-1 z-[60]"
          onCloseAutoFocus={(e) => {
            const action = pending.current;
            pending.current = null;
            if (!action) return;
            e.preventDefault();
            trigger.current?.focus();
            action();
          }}
        >
          {onUseMusicBrainzCover && (
            <DropdownMenu.Item className={item} onSelect={() => { pending.current = onUseMusicBrainzCover; }}>
              <ImagePlus className="w-3.5 h-3.5" /> Use MusicBrainz cover
            </DropdownMenu.Item>
          )}
          <DropdownMenu.Item className={item} onSelect={() => { pending.current = onUpload; }}>
            <Upload className="w-3.5 h-3.5" /> Upload, paste or URL…
          </DropdownMenu.Item>
          {onAskAiForCover && (
            <DropdownMenu.Item className={item} onSelect={() => { pending.current = onAskAiForCover; }}>
              <Sparkles className="w-3.5 h-3.5" /> Ask AI to find a cover…
            </DropdownMenu.Item>
          )}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function formatDuration(seconds: number): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = Math.floor(seconds % 60);
  if (h > 0) return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`;
  return `${m}:${s.toString().padStart(2, "0")}`;
}
