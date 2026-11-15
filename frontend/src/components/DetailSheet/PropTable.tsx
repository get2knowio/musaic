import { useState } from "react";
import { ExternalLink, Loader2 } from "lucide-react";
import { toast } from "sonner";

import RevealInFileManager from "@/components/RevealInFileManager";
import { clearAlbumMbAnchor } from "@/lib/api";
import type { Album } from "@/lib/types";

interface PropTableProps {
  album: Album;
  /** A model backend can run (lib/aiGating.ts). Without one the row and
   *  the confirm text don't name AI or hints. */
  aiReady?: boolean;
}

/**
 * Album info section body. Slimmed-down to fields not already shown in the
 * AlbumHero — drops track count / length / "from folder" duplication.
 *
 * The format row pulls from the album-level FLAC StreamInfo we now store
 * (assumes all tracks share the same audio format, which holds for
 * CD-rips and LP-rips).
 */
export default function PropTable({ album, aiReady = false }: PropTableProps) {
  return (
    <dl className="grid grid-cols-[110px_1fr] gap-y-2 gap-x-3 px-4 py-3 text-sm">
      <Row label="format">{formatString(album)}</Row>
      <Row label="cover art">
        {album.has_embedded_cover ? (
          <>
            embedded in FLAC
            {album.cover_file && (
              <span className="text-fg-4 ml-1.5 text-xs">
                (also <code className="mono">{album.cover_file}</code> on disk)
              </span>
            )}
          </>
        ) : album.cover_file ? (
          <code className="mono text-xs">{album.cover_file}</code>
        ) : (
          <span className="text-fg-4">none</span>
        )}
      </Row>
      <Row label="MusicBrainz"><MusicBrainzCell album={album} aiReady={aiReady} /></Row>
      <Row label={aiReady ? "AI reviewed" : "Reviewed"}>{aiReviewedCell(album)}</Row>
      <Row label="folder">
        <code className="mono text-xs text-fg-3 break-all">{album.path}</code>
        {/* Desktop window only; a Missing album's folder is
            not on disk, so nothing to show. */}
        {!album.missing_since && (
          <RevealInFileManager target={{ kind: "album", albumId: album.id }} className="ml-2 align-baseline" />
        )}
      </Row>
    </dl>
  );
}

/** MusicBrainz status cell. Three states keyed off (mbid, checked_at):
 *
 *   - mbid present                       → "<uuid>" + "(checked …)" + Wrong match
 *   - mbid null, checked_at present      → "Not found" + "(last checked …)"
 *   - both null                          → "Not yet checked"
 *
 * The "Wrong match" affordance is the same escape hatch the AI-suggestions
 * panel surfaces: it clears the cached anchor AND adds the MBID to the
 * album's blocklist so future automatic resolution can't pick the same
 * wrong release-group. Inlined here so the user can clear without first
 * having to run an AI job to surface the post-suggestion control.
 */
function MusicBrainzCell({ album, aiReady }: { album: Album; aiReady: boolean }) {
  const [clearing, setClearing] = useState(false);

  const mbid = album.mb_release_group_mbid ?? null;
  const checkedAt = album.mb_last_checked_at ?? null;

  const onClear = async () => {
    if (clearing || !mbid) return;
    // Lightweight confirm — the action is recoverable (the user can re-run
    // AI / the resolver to land a new anchor), but blocklisting is a real
    // side effect and a misclick shouldn't silently drop a good match.
    const ok = window.confirm(
      `Clear MusicBrainz match for this album?\n\n` +
      `${mbid}\n\n` +
      `This release-group will be added to the album's blocklist so ` +
      `automatic resolution can't pick it again. ` +
      (aiReady
        ? `You can re-identify the album with a hint via "Suggest fixes".`
        : `The next lookup searches MusicBrainz again.`),
    );
    if (!ok) return;
    setClearing(true);
    try {
      const res = await clearAlbumMbAnchor(album.id);
      const msg = res.blocked_mbid
        ? `Cleared and blocklisted ${res.blocked_mbid.slice(0, 8)}…`
        : "MusicBrainz anchor cleared";
      toast.success(msg, { duration: 4500 });
      // Cache invalidation is handled by the App-level
      // useLibraryEventsStream subscriber — the backend publishes
      // AlbumTagsChanged after wiping the anchor.
    } catch (err) {
      toast.error((err as Error).message || "Couldn't clear MusicBrainz anchor");
    } finally {
      setClearing(false);
    }
  };

  if (mbid) {
    return (
      <span className="inline-flex flex-wrap items-baseline gap-1.5">
        <a
          href={`https://musicbrainz.org/release-group/${mbid}`}
          target="_blank"
          rel="noreferrer"
          title={`Open release-group ${mbid} on MusicBrainz`}
          className="mono text-xs text-accent hover:underline break-all inline-flex items-baseline gap-1"
        >
          {mbid}
          <ExternalLink className="w-3 h-3 self-center shrink-0" />
        </a>
        {checkedAt && (
          <span className="text-fg-4 text-xs">
            (checked {formatLocal(checkedAt)})
          </span>
        )}
        <button
          type="button"
          onClick={onClear}
          disabled={clearing}
          className="text-xs text-fg-4 hover:text-err transition disabled:opacity-60 inline-flex items-baseline gap-1"
          title="Clear this MusicBrainz match and blocklist it for this album"
        >
          {clearing ? (
            <><Loader2 className="w-3 h-3 spin self-center" /> Clearing…</>
          ) : (
            "Wrong match?"
          )}
        </button>
      </span>
    );
  }
  if (checkedAt) {
    return (
      <span className="inline-flex items-baseline gap-1.5">
        <span className="text-fg-3 italic">Not found</span>
        <span className="text-fg-4 text-xs">
          (last checked {formatLocal(checkedAt)})
        </span>
      </span>
    );
  }
  return <span className="text-fg-4">not yet checked</span>;
}

function aiReviewedCell(album: Album): React.ReactNode {
  const at = album.metadata_ai_acted_on_at ?? null;
  if (at) {
    return (
      <span className="inline-flex items-baseline gap-1.5">
        <span className="text-fg-2">Yes</span>
        <span className="text-fg-4 text-xs">({formatLocal(at)})</span>
      </span>
    );
  }
  return <span className="text-fg-4">no</span>;
}

function formatLocal(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleDateString();
  } catch {
    return iso;
  }
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="mono text-xs uppercase tracking-[0.06em] text-fg-4 self-baseline">
        {label}
      </dt>
      <dd className="text-sm text-fg-2">{children}</dd>
    </>
  );
}

function formatString(album: Album): string {
  const parts: string[] = ["FLAC"];
  if (album.sample_rate) {
    const khz = album.sample_rate / 1000;
    parts.push(Number.isInteger(khz) ? `${khz} kHz` : `${khz.toFixed(1)} kHz`);
  }
  if (album.bits_per_sample) parts.push(`${album.bits_per_sample}-bit`);
  if (album.channels) parts.push(album.channels === 1 ? "mono" : album.channels === 2 ? "stereo" : `${album.channels}ch`);
  return parts.join(" · ");
}
