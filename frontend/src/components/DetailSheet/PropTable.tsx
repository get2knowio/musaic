import type { Album } from "@/lib/types";

interface PropTableProps {
  album: Album;
}

/**
 * Album info section body. Slimmed-down to fields not already shown in the
 * AlbumHero — drops track count / length / "from folder" duplication.
 *
 * The format row pulls from the album-level FLAC StreamInfo we now store
 * (assumes all tracks share the same audio format, which holds for
 * CD-rips and LP-rips).
 */
export default function PropTable({ album }: PropTableProps) {
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
      <Row label="MusicBrainz">{musicBrainzCell(album)}</Row>
      <Row label="AI reviewed">{aiReviewedCell(album)}</Row>
      <Row label="folder">
        <code className="mono text-xs text-fg-3 break-all">{album.path}</code>
      </Row>
    </dl>
  );
}

/** MusicBrainz status cell. Three states keyed off (mbid, checked_at):
 *
 *   - mbid present                       → "<uuid>" + "(checked …)"
 *   - mbid null, checked_at present      → "Not found" + "(last checked …)"
 *   - both null                          → "Not yet checked"
 *
 * Telling these apart matters for the user — "Not found" + "checked
 * recently" is a stable state (don't bother re-running AI), while
 * "Not yet checked" signals a free win available via the missing-mbid
 * Fix button.
 */
function musicBrainzCell(album: Album): React.ReactNode {
  const mbid = album.mb_release_group_mbid ?? null;
  const checkedAt = album.mb_last_checked_at ?? null;
  if (mbid) {
    return (
      <span className="inline-flex flex-wrap items-baseline gap-1.5">
        <code className="mono text-xs text-fg-3 break-all" title={`release-group: ${mbid}`}>
          {mbid}
        </code>
        {checkedAt && (
          <span className="text-fg-4 text-xs">
            (checked {formatLocal(checkedAt)})
          </span>
        )}
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
