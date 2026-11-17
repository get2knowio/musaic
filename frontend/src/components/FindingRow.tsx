import { Link } from "react-router-dom";
import { AlertCircle, ArrowUpRight, Info, XCircle, Zap } from "lucide-react";
import { toast } from "sonner";

import { useUnignoreFinding } from "@/hooks/useChecks";
import FindingFixButton from "@/components/FindingFixButton";
import IgnoreFindingControl from "@/components/IgnoreFindingControl";
import { displayArtist } from "@/lib/albumFields";
import { GENRE_HINT_POINTER, genreHintLead, suggestedGenre } from "@/lib/genreHint";
import type { Album, Finding } from "@/lib/types";

export function FindingRow({
  checkId,
  finding,
  fixKind,
  album,
  siblings,
}: {
  checkId: string;
  finding: Finding;
  fixKind: "none" | "ai-job" | "deterministic";
  /** Looked up from the album cache by the parent. Null = library-level
   *  finding OR the album row was deleted out from under it. */
  album: Album | null;
  /** All findings currently visible in the parent's list. Threaded
   *  through to FindingFixButton so cluster-resolve dialogs can
   *  chain through them after each apply. */
  siblings: Finding[];
}) {
  const unignoreMutation = useUnignoreFinding();

  const onUnignore = () => {
    unignoreMutation.mutate(
      { checkId, findingId: finding.id },
      { onError: (e) => toast.error((e as Error).message || "Couldn't restore") },
    );
  };

  const genre = suggestedGenre(finding);

  const SevIcon = finding.severity === "error" ? XCircle : finding.severity === "info" ? Info : AlertCircle;
  const sevClass = finding.severity === "error" ? "text-err" : finding.severity === "info" ? "text-fg-3" : "text-warn";

  // Display name for the row's primary line.
  //
  // Most checks point at one specific album, so showing
  // "Title — Artist" is the natural headline. But some checks
  // describe a CLUSTER (e.g. inconsistent-artist-sort): the
  // album_id is just a deterministic representative pick for stable
  // upserting, and headlining one of the cluster's albums obscures
  // what the finding is actually about. Those checks include a
  // `headline` field in detail that we prefer over the album line.
  // Falls back to "unknown album · <id stub>" for stale findings
  // whose album disappeared since the last scan.
  const detailHeadline =
    finding.detail && typeof finding.detail.headline === "string"
      ? finding.detail.headline
      : null;
  const albumLine = detailHeadline
    ? detailHeadline
    : album
    ? formatAlbumLine(album)
    : finding.album_id
    ? `unknown album · ${finding.album_id.slice(0, 8)}`
    : "—";

  return (
    <div className={`px-3 py-2 flex items-start gap-3 ${finding.ignored_at ? "opacity-55" : ""}`}>
      <SevIcon className={`w-4 h-4 shrink-0 mt-0.5 ${sevClass}`} />
      <div className="flex-1 min-w-0">
        {/* Title row + jump-to-album icon. Same pattern as the Jobs
            page: the title itself isn't a link (keeping the visual
            consistent with non-clickable text), an inline
            ArrowUpRight icon jumps to /library/{album_id}. The route
            mounts LibraryPage with the album detail sheet open. */}
        <div className="text-sm flex items-center gap-1.5 min-w-0">
          <span
            className={
              "font-medium truncate "
              + (finding.album_id ? "" : "text-fg-3")
            }
            title={album?.path || finding.album_id || undefined}
          >
            {albumLine}
          </span>
          {finding.album_id && (
            <Link
              to={`/library/${encodeURIComponent(finding.album_id)}`}
              className="shrink-0 inline-flex items-center justify-center w-5 h-5 rounded text-fg-4 hover:text-accent hover:bg-bg-3 transition"
              title="View album detail"
              aria-label="View album detail"
            >
              <ArrowUpRight className="w-3.5 h-3.5" />
            </Link>
          )}
        </div>
        <div className="mono text-[11px] text-fg-4 mt-0.5 flex items-center gap-2 flex-wrap">
          <span className="text-fg-3 normal-case tracking-normal">
            {finding.summary}
          </span>
          <span>·</span>
          <span>seen {fmtRelative(finding.last_seen_at)}</span>
          {finding.ignored_reason && (
            <>
              <span>·</span>
              <span className="italic truncate" title={finding.ignored_reason}>
                ignored: {finding.ignored_reason}
              </span>
            </>
          )}
        </div>
        {genre && finding.album_id && !album?.missing_since && (
          // The album's corrections list is in the panel the link above
          // opens. A Missing album's panel shows none, so no line for it.
          <div className="text-[11px] text-fg-3 mt-0.5">
            {genreHintLead(genre)}{" "}
            <Link
              to={`/library/${encodeURIComponent(finding.album_id)}`}
              className="text-accent hover:underline"
            >
              {GENRE_HINT_POINTER}
            </Link>
          </div>
        )}
      </div>
      <div className="flex items-center gap-1 shrink-0">
        {album?.missing_since && fixKind !== "none" && !finding.ignored_at ? (
          // A Missing album's folder is not on disk, so no fix can touch
          // its files; the album sheet hides findings for the same reason.
          <button
            type="button"
            disabled
            className="h-7 px-2 rounded border border-line text-xs text-fg-3 inline-flex items-center gap-1 cursor-not-allowed"
            title="This album's folder is missing. Fix is available again once the album is back in the library."
          >
            <Zap className="w-3 h-3" />
            Fix
          </button>
        ) : (
          <FindingFixButton finding={finding} fixKind={fixKind} siblings={siblings} />
        )}
        {finding.ignored_at ? (
          <button
            onClick={onUnignore}
            disabled={unignoreMutation.isPending}
            className="h-7 px-2 rounded border border-line text-xs text-fg-2 hover:bg-bg-3 hover:text-fg transition disabled:opacity-60"
          >
            Restore
          </button>
        ) : (
          <IgnoreFindingControl checkId={checkId} findingId={finding.id} />
        )}
      </div>
    </div>
  );
}

function fmtRelative(iso: string | null): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const diffSec = (Date.now() - t) / 1000;
  if (diffSec < 60) return "just now";
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}

/** Display label for an album in a finding row. Mirrors the artist logic in
 *  Tile / AlbumList: "Various Artists" for compilations, otherwise
 *  album_artist || artist. Falls back to dir_name when both are blank. */
function formatAlbumLine(a: Album): string {
  const artist = a.is_compilation
    ? "Various Artists"
    : displayArtist(a);
  const title = (a.title || a.dir_name || "").trim();
  if (artist && title) return `${title} — ${artist}`;
  return title || a.dir_name || a.id;
}
